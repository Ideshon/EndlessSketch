use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use eframe::egui::Pos2;

const CURSOR_SAMPLE_INTERVAL_MS: u64 = 2;
const CURSOR_SAMPLE_CAPACITY: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MouseSample {
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct NativeWindow(isize);

#[derive(Default)]
pub struct MouseHistory {
    sampler: Option<CursorSampler>,
    last_mode: MouseHistoryMode,
    last_sample_count: usize,
}

#[derive(Default)]
enum MouseHistoryMode {
    #[default]
    Unavailable,
    CursorSampler,
    TouchEvents,
}

struct CursorSampler {
    samples: Arc<Mutex<VecDeque<MouseSample>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CursorSampler {
    fn drain(&self) -> Option<Vec<MouseSample>> {
        let mut samples = self.samples.lock().ok()?;
        Some(samples.drain(..).collect())
    }
}

impl Drop for CursorSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl MouseHistory {
    pub fn begin(&mut self, window: Option<NativeWindow>, _position: Pos2, pixels_per_point: f32) {
        self.reset();
        self.last_mode = MouseHistoryMode::Unavailable;
        self.last_sample_count = 0;
        if window.is_some()
            && pixels_per_point.is_finite()
            && pixels_per_point > f32::EPSILON
            && let Some(sampler) = start_cursor_sampler()
        {
            self.sampler = Some(sampler);
            self.last_mode = MouseHistoryMode::CursorSampler;
        }
    }

    pub fn positions_since(
        &mut self,
        window: Option<NativeWindow>,
        _position: Pos2,
        pixels_per_point: f32,
    ) -> Option<Vec<Pos2>> {
        let window = window?;
        if !pixels_per_point.is_finite() || pixels_per_point <= f32::EPSILON {
            return None;
        }
        let samples = self.sampler.as_ref()?.drain()?;
        let points = samples
            .into_iter()
            .filter_map(|sample| sample_to_egui(window, sample, pixels_per_point))
            .fold(Vec::new(), |mut points, point| {
                if points.last() != Some(&point) {
                    points.push(point);
                }
                points
            });
        if points.is_empty() {
            return None;
        }
        self.last_sample_count = self.last_sample_count.saturating_add(points.len());
        Some(points)
    }

    pub fn reset(&mut self) {
        self.sampler = None;
    }

    pub fn record_touch_events(&mut self, sample_count: usize) {
        self.reset();
        self.last_mode = MouseHistoryMode::TouchEvents;
        self.last_sample_count = self.last_sample_count.saturating_add(sample_count);
    }

    pub fn diagnostics(&self) -> (&'static str, usize) {
        let mode = match self.last_mode {
            MouseHistoryMode::Unavailable => "unavailable",
            MouseHistoryMode::CursorSampler => "cursor_sampler",
            MouseHistoryMode::TouchEvents => "touch_events",
        };
        (mode, self.last_sample_count)
    }
}

fn push_changed_sample(samples: &mut VecDeque<MouseSample>, sample: MouseSample) {
    if samples.back() == Some(&sample) {
        return;
    }
    if samples.len() == CURSOR_SAMPLE_CAPACITY {
        samples.pop_front();
    }
    samples.push_back(sample);
}

#[cfg(target_os = "windows")]
fn start_cursor_sampler() -> Option<CursorSampler> {
    use std::thread;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let samples = Arc::new(Mutex::new(VecDeque::with_capacity(512)));
    let stop = Arc::new(AtomicBool::new(false));
    let worker_samples = Arc::clone(&samples);
    let worker_stop = Arc::clone(&stop);
    let worker = thread::Builder::new()
        .name("endless-sketch-cursor-sampler".to_owned())
        .spawn(move || {
            let mut saw_primary_down = false;
            let mut initial_up_checks = 0;
            while !worker_stop.load(Ordering::Acquire) {
                // SAFETY: GetAsyncKeyState reads process-independent key state.
                let primary_down =
                    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) } as u16 & 0x8000 != 0;
                if !primary_down {
                    if saw_primary_down || initial_up_checks >= 4 {
                        break;
                    }
                    initial_up_checks += 1;
                    thread::sleep(Duration::from_millis(CURSOR_SAMPLE_INTERVAL_MS));
                    continue;
                }
                saw_primary_down = true;
                let mut point = POINT::default();
                // SAFETY: point is valid writable memory for GetCursorPos.
                if unsafe { GetCursorPos(&mut point) } != 0
                    && let Ok(mut samples) = worker_samples.lock()
                {
                    push_changed_sample(
                        &mut samples,
                        MouseSample {
                            x: point.x,
                            y: point.y,
                        },
                    );
                }
                thread::sleep(Duration::from_millis(CURSOR_SAMPLE_INTERVAL_MS));
            }
        })
        .ok()?;
    Some(CursorSampler {
        samples,
        stop,
        worker: Some(worker),
    })
}

#[cfg(not(target_os = "windows"))]
fn start_cursor_sampler() -> Option<CursorSampler> {
    None
}

pub fn native_window(frame: &eframe::Frame) -> Option<NativeWindow> {
    native_window_impl(frame)
}

#[cfg(target_os = "windows")]
fn native_window_impl(frame: &eframe::Frame) -> Option<NativeWindow> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = frame.window_handle().ok()?.as_raw();
    match handle {
        RawWindowHandle::Win32(handle) => Some(NativeWindow(handle.hwnd.get())),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn native_window_impl(_frame: &eframe::Frame) -> Option<NativeWindow> {
    None
}

#[cfg(target_os = "windows")]
fn sample_to_egui(
    window: NativeWindow,
    sample: MouseSample,
    pixels_per_point: f32,
) -> Option<Pos2> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;

    let hwnd = window.0 as windows_sys::Win32::Foundation::HWND;
    let mut client = POINT {
        x: sample.x,
        y: sample.y,
    };
    // SAFETY: hwnd belongs to the active eframe window and client is writable memory.
    if unsafe { ScreenToClient(hwnd, &mut client) } == 0 {
        return None;
    }
    Some(Pos2::new(
        client.x as f32 / pixels_per_point,
        client.y as f32 / pixels_per_point,
    ))
}

#[cfg(not(target_os = "windows"))]
fn sample_to_egui(
    _window: NativeWindow,
    _sample: MouseSample,
    _pixels_per_point: f32,
) -> Option<Pos2> {
    None
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{CURSOR_SAMPLE_CAPACITY, MouseSample, push_changed_sample};

    #[test]
    fn cursor_sampler_keeps_changed_positions_in_order_and_bounded() {
        let mut samples = VecDeque::new();
        push_changed_sample(&mut samples, MouseSample { x: 1, y: 2 });
        push_changed_sample(&mut samples, MouseSample { x: 1, y: 2 });
        push_changed_sample(&mut samples, MouseSample { x: 3, y: 4 });
        assert_eq!(
            samples.iter().copied().collect::<Vec<_>>(),
            vec![MouseSample { x: 1, y: 2 }, MouseSample { x: 3, y: 4 }]
        );

        for coordinate in 0..=CURSOR_SAMPLE_CAPACITY {
            push_changed_sample(
                &mut samples,
                MouseSample {
                    x: coordinate as i32 + 10,
                    y: 0,
                },
            );
        }
        assert_eq!(samples.len(), CURSOR_SAMPLE_CAPACITY);
        assert_eq!(samples.back().map(|sample| sample.x), Some(16_394));
    }
}
