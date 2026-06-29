use eframe::egui::Pos2;

const HISTORY_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MouseSample {
    x: i32,
    y: i32,
    time: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct NativeWindow(isize);

#[derive(Default)]
pub struct MouseHistory {
    marker: Option<MouseSample>,
}

impl MouseHistory {
    pub fn begin(&mut self, window: Option<NativeWindow>, position: Pos2, pixels_per_point: f32) {
        self.marker = window
            .and_then(|window| query_history(window, position, pixels_per_point))
            .and_then(|samples| samples.first().copied());
    }

    pub fn positions_since(
        &mut self,
        window: Option<NativeWindow>,
        position: Pos2,
        pixels_per_point: f32,
    ) -> Option<Vec<Pos2>> {
        let window = window?;
        let samples = query_history(window, position, pixels_per_point)?;
        let newest = samples.first().copied()?;
        let Some(marker) = self.marker else {
            self.marker = Some(newest);
            return None;
        };
        let Some(chronological) = samples_after_marker(&samples, marker) else {
            self.marker = Some(newest);
            return None;
        };

        self.marker = Some(newest);
        Some(
            chronological
                .into_iter()
                .filter_map(|sample| sample_to_egui(window, sample, pixels_per_point))
                .fold(Vec::new(), |mut points, point| {
                    if points.last() != Some(&point) {
                        points.push(point);
                    }
                    points
                }),
        )
    }

    pub fn reset(&mut self) {
        self.marker = None;
    }
}

fn samples_after_marker(
    newest_first: &[MouseSample],
    marker: MouseSample,
) -> Option<Vec<MouseSample>> {
    let marker_index = newest_first.iter().position(|sample| *sample == marker)?;
    Some(newest_first[..marker_index].iter().rev().copied().collect())
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
fn query_history(
    window: NativeWindow,
    position: Pos2,
    pixels_per_point: f32,
) -> Option<Vec<MouseSample>> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GMMP_USE_DISPLAY_POINTS, GetMouseMovePointsEx, MOUSEMOVEPOINT,
    };

    if !pixels_per_point.is_finite() || pixels_per_point <= f32::EPSILON {
        return None;
    }
    let hwnd = window.0 as windows_sys::Win32::Foundation::HWND;
    let mut screen = POINT {
        x: (position.x * pixels_per_point).round() as i32,
        y: (position.y * pixels_per_point).round() as i32,
    };
    // SAFETY: hwnd belongs to the active eframe window and screen points to valid memory.
    if unsafe { ClientToScreen(hwnd, &mut screen) } == 0 {
        return None;
    }

    let input = MOUSEMOVEPOINT {
        x: screen.x & 0x0000_FFFF,
        y: screen.y & 0x0000_FFFF,
        ..Default::default()
    };
    let mut output = [MOUSEMOVEPOINT::default(); HISTORY_CAPACITY];
    // SAFETY: input and the fixed-size output buffer are valid for the requested count.
    let count = unsafe {
        GetMouseMovePointsEx(
            size_of::<MOUSEMOVEPOINT>() as u32,
            &input,
            output.as_mut_ptr(),
            HISTORY_CAPACITY as i32,
            GMMP_USE_DISPLAY_POINTS,
        )
    };
    if count <= 0 {
        return None;
    }

    Some(
        output[..count as usize]
            .iter()
            .map(|point| MouseSample {
                x: sign_extend_display_coordinate(point.x),
                y: sign_extend_display_coordinate(point.y),
                time: point.time,
            })
            .collect(),
    )
}

#[cfg(not(target_os = "windows"))]
fn query_history(
    _window: NativeWindow,
    _position: Pos2,
    _pixels_per_point: f32,
) -> Option<Vec<MouseSample>> {
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
    // SAFETY: hwnd belongs to the active eframe window and client points to valid memory.
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

fn sign_extend_display_coordinate(coordinate: i32) -> i32 {
    let coordinate = coordinate & 0x0000_FFFF;
    if coordinate > i16::MAX as i32 {
        coordinate - 65_536
    } else {
        coordinate
    }
}

#[cfg(test)]
mod tests {
    use super::{MouseSample, samples_after_marker, sign_extend_display_coordinate};

    fn sample(x: i32, time: u32) -> MouseSample {
        MouseSample { x, y: x * 2, time }
    }

    #[test]
    fn display_coordinates_are_sign_extended_from_sixteen_bits() {
        assert_eq!(sign_extend_display_coordinate(12_345), 12_345);
        assert_eq!(sign_extend_display_coordinate(0xFFFF), -1);
        assert_eq!(sign_extend_display_coordinate(0x8000), i16::MIN as i32);
        assert_eq!(sign_extend_display_coordinate(0x1_FFFE), -2);
    }

    #[test]
    fn history_keeps_only_new_samples_in_chronological_order() {
        let history = [sample(5, 50), sample(4, 40), sample(3, 30), sample(2, 20)];

        assert_eq!(
            samples_after_marker(&history, sample(3, 30)),
            Some(vec![sample(4, 40), sample(5, 50)])
        );
        assert_eq!(
            samples_after_marker(&history, sample(5, 50)),
            Some(Vec::new())
        );
        assert_eq!(samples_after_marker(&history, sample(1, 10)), None);
    }
}
