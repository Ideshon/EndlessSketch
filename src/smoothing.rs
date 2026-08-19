pub const STROKE_SMOOTHING_PASSES: usize = 2;
pub const DENSE_RENDER_POINT_THRESHOLD: usize = 256;
pub const RENDER_SIMPLIFICATION_TOLERANCE_PX: f32 = 0.25;
const MAX_JITTER_PREFILTER_POINTS: usize = 8192;
const JITTER_PREFILTER_CHUNK_POINTS: usize = 64;
const JITTER_PREFILTER_CHUNK_LENGTH_PX: f32 = 32.0;
const COLLINEAR_RELATIVE_TOLERANCE: f32 = 1.0e-4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometryClipRect {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl GeometryClipRect {
    pub fn new(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }
}

pub fn simplify_render_points(points: &[(f32, f32)], closed: bool) -> Vec<(f32, f32)> {
    if points.len() <= DENSE_RENDER_POINT_THRESHOLD {
        return points.to_vec();
    }

    simplify_render_points_with_tolerance(points, closed, RENDER_SIMPLIFICATION_TOLERANCE_PX)
}

pub(crate) fn simplify_render_points_with_tolerance(
    points: &[(f32, f32)],
    closed: bool,
    tolerance: f32,
) -> Vec<(f32, f32)> {
    let mut source = points;
    if closed && points.len() > 1 && points.first() == points.last() {
        source = &points[..points.len() - 1];
    }
    let minimum = if closed { 3 } else { 2 };
    if source.len() <= minimum {
        return source.to_vec();
    }

    let tolerance_squared = tolerance * tolerance;
    let mut simplified = Vec::with_capacity(source.len());
    simplified.push(source[0]);
    let end = if closed {
        source.len()
    } else {
        source.len() - 1
    };
    for &point in &source[1..end] {
        if point_distance_squared(*simplified.last().expect("first point exists"), point)
            <= tolerance_squared
        {
            continue;
        }
        while simplified.len() >= 2 {
            let middle = simplified[simplified.len() - 1];
            let start = simplified[simplified.len() - 2];
            if distance_to_segment(middle, start, point) > tolerance {
                break;
            }
            simplified.pop();
        }
        simplified.push(point);
    }
    if !closed {
        let last = *source.last().expect("source has at least two points");
        if simplified.last() != Some(&last) {
            simplified.push(last);
        }
    }

    if simplified.len() < minimum {
        source.to_vec()
    } else {
        simplified
    }
}

pub fn clip_polyline_to_rect(
    points: &[(f32, f32)],
    rect: GeometryClipRect,
) -> Vec<Vec<(f32, f32)>> {
    let mut runs = Vec::new();
    let mut current = Vec::new();
    for segment in points.windows(2) {
        let Some((start, end)) = clip_segment_to_rect(segment[0], segment[1], rect) else {
            if current.len() >= 2 {
                runs.push(std::mem::take(&mut current));
            }
            continue;
        };
        if current
            .last()
            .is_some_and(|last| points_are_close(*last, start))
        {
            if !points_are_close(*current.last().expect("checked last"), end) {
                current.push(end);
            }
        } else {
            if current.len() >= 2 {
                runs.push(std::mem::take(&mut current));
            }
            current.push(start);
            if !points_are_close(start, end) {
                current.push(end);
            }
        }
    }
    if current.len() >= 2 {
        runs.push(current);
    }
    runs
}

pub fn clip_polygon_to_rect(points: &[(f32, f32)], rect: GeometryClipRect) -> Vec<(f32, f32)> {
    let mut output = points.to_vec();
    if output.len() > 1 && output.first() == output.last() {
        output.pop();
    }
    for edge in [
        ClipEdge::Left,
        ClipEdge::Right,
        ClipEdge::Top,
        ClipEdge::Bottom,
    ] {
        if output.is_empty() {
            break;
        }
        let input = std::mem::take(&mut output);
        let mut previous = *input.last().expect("non-empty polygon");
        let mut previous_inside = edge.inside(previous, rect);
        for current in input {
            let current_inside = edge.inside(current, rect);
            if current_inside != previous_inside
                && let Some(intersection) = edge.intersection(previous, current, rect)
            {
                output.push(intersection);
            }
            if current_inside {
                output.push(current);
            }
            previous = current;
            previous_inside = current_inside;
        }
    }
    output
}

#[derive(Debug, Clone, Copy)]
enum ClipEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl ClipEdge {
    fn inside(self, point: (f32, f32), rect: GeometryClipRect) -> bool {
        match self {
            Self::Left => point.0 >= rect.min_x,
            Self::Right => point.0 <= rect.max_x,
            Self::Top => point.1 >= rect.min_y,
            Self::Bottom => point.1 <= rect.max_y,
        }
    }

    fn intersection(
        self,
        start: (f32, f32),
        end: (f32, f32),
        rect: GeometryClipRect,
    ) -> Option<(f32, f32)> {
        let dx = end.0 - start.0;
        let dy = end.1 - start.1;
        let t = match self {
            Self::Left if dx.abs() > f32::EPSILON => (rect.min_x - start.0) / dx,
            Self::Right if dx.abs() > f32::EPSILON => (rect.max_x - start.0) / dx,
            Self::Top if dy.abs() > f32::EPSILON => (rect.min_y - start.1) / dy,
            Self::Bottom if dy.abs() > f32::EPSILON => (rect.max_y - start.1) / dy,
            _ => return None,
        };
        Some((start.0 + dx * t, start.1 + dy * t))
    }
}

fn clip_segment_to_rect(
    start: (f32, f32),
    end: (f32, f32),
    rect: GeometryClipRect,
) -> Option<((f32, f32), (f32, f32))> {
    if !start.0.is_finite() || !start.1.is_finite() || !end.0.is_finite() || !end.1.is_finite() {
        return None;
    }
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let mut entry = 0.0_f32;
    let mut exit = 1.0_f32;
    for (p, q) in [
        (-dx, start.0 - rect.min_x),
        (dx, rect.max_x - start.0),
        (-dy, start.1 - rect.min_y),
        (dy, rect.max_y - start.1),
    ] {
        if p.abs() <= f32::EPSILON {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let ratio = q / p;
        if p < 0.0 {
            entry = entry.max(ratio);
        } else {
            exit = exit.min(ratio);
        }
        if entry > exit {
            return None;
        }
    }
    Some((
        (start.0 + dx * entry, start.1 + dy * entry),
        (start.0 + dx * exit, start.1 + dy * exit),
    ))
}

fn distance_to_segment(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let length_squared = dx * dx + dy * dy;
    if length_squared <= f32::EPSILON {
        return point_distance_squared(point, start).sqrt();
    }
    let t =
        (((point.0 - start.0) * dx + (point.1 - start.1) * dy) / length_squared).clamp(0.0, 1.0);
    let closest = (start.0 + t * dx, start.1 + t * dy);
    point_distance_squared(point, closest).sqrt()
}

fn point_distance_squared(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

fn points_are_close(a: (f32, f32), b: (f32, f32)) -> bool {
    point_distance_squared(a, b) <= 1e-6
}

pub fn smooth_stroke_points(points: &[(f32, f32)], passes: usize) -> Vec<(f32, f32)> {
    if points.len() <= 2 || passes == 0 {
        return points.to_vec();
    }

    let parameters = curve_parameters(passes);
    let filtered = prefilter_smoothing_points(points, false, parameters.jitter_tolerance);
    let points = filtered.as_slice();
    if points.len() <= 2 {
        return filtered;
    }
    let mut smoothed = Vec::with_capacity(points.len() * 2);
    smoothed.push(points[0]);
    for index in 1..points.len() - 1 {
        append_smoothed_corner(
            &mut smoothed,
            points[index - 1],
            points[index],
            points[index + 1],
            parameters,
        );
    }
    push_unique(
        &mut smoothed,
        *points.last().expect("stroke has an endpoint"),
    );
    smoothed
}

pub fn smooth_stroke_points_stable(points: &[(f32, f32)], passes: usize) -> Vec<(f32, f32)> {
    if points.len() <= 2 || passes == 0 {
        return points.to_vec();
    }

    let collapsed = collapse_collinear_points(points, false);
    let points = collapsed.as_slice();
    if points.len() <= 2 {
        return collapsed;
    }
    let parameters = curve_parameters(passes);
    let mut smoothed = Vec::with_capacity(points.len() * stable_corner_steps(passes));
    smoothed.push(points[0]);
    for index in 1..points.len() - 1 {
        append_stable_smoothed_corner(
            &mut smoothed,
            points[index - 1],
            points[index],
            points[index + 1],
            parameters,
            passes,
        );
    }
    push_unique(
        &mut smoothed,
        *points.last().expect("stroke has an endpoint"),
    );
    smoothed
}

pub fn smooth_closed_points(points: &[(f32, f32)], passes: usize) -> Vec<(f32, f32)> {
    let mut ring = points;
    if ring.len() > 1 && ring.first() == ring.last() {
        ring = &ring[..ring.len() - 1];
    }
    if ring.len() < 3 || passes == 0 {
        return ring.to_vec();
    }

    let parameters = curve_parameters(passes);
    let filtered = prefilter_smoothing_points(ring, true, parameters.jitter_tolerance);
    let ring = filtered.as_slice();
    if ring.len() < 3 {
        return filtered;
    }
    let mut smoothed = Vec::with_capacity(ring.len() * 2);
    for index in 0..ring.len() {
        append_smoothed_corner(
            &mut smoothed,
            ring[(index + ring.len() - 1) % ring.len()],
            ring[index],
            ring[(index + 1) % ring.len()],
            parameters,
        );
    }
    smoothed
}

pub fn smooth_closed_points_stable(points: &[(f32, f32)], passes: usize) -> Vec<(f32, f32)> {
    let mut ring = points;
    if ring.len() > 1 && ring.first() == ring.last() {
        ring = &ring[..ring.len() - 1];
    }
    if ring.len() < 3 || passes == 0 {
        return ring.to_vec();
    }

    let collapsed = collapse_collinear_points(ring, true);
    let ring = collapsed.as_slice();
    if ring.len() < 3 {
        return collapsed;
    }
    let parameters = curve_parameters(passes);
    let mut smoothed = Vec::with_capacity(ring.len() * stable_corner_steps(passes));
    for index in 0..ring.len() {
        append_stable_smoothed_corner(
            &mut smoothed,
            ring[(index + ring.len() - 1) % ring.len()],
            ring[index],
            ring[(index + 1) % ring.len()],
            parameters,
            passes,
        );
    }
    smoothed
}

fn collapse_collinear_points(points: &[(f32, f32)], closed: bool) -> Vec<(f32, f32)> {
    let mut source = points;
    if closed && source.len() > 1 && source.first() == source.last() {
        source = &source[..source.len() - 1];
    }
    let minimum = if closed { 3 } else { 2 };
    if source.len() <= minimum {
        return source.to_vec();
    }

    let mut collapsed = Vec::with_capacity(source.len());
    for &point in source {
        collapsed.push(point);
        while collapsed.len() >= 3 {
            let end = collapsed.len() - 1;
            if !collinear_middle_is_redundant(
                collapsed[end - 2],
                collapsed[end - 1],
                collapsed[end],
            ) {
                break;
            }
            collapsed.remove(end - 1);
        }
    }

    if closed {
        while collapsed.len() > minimum {
            let removable = (0..collapsed.len()).find(|&index| {
                let len = collapsed.len();
                collinear_middle_is_redundant(
                    collapsed[(index + len - 1) % len],
                    collapsed[index],
                    collapsed[(index + 1) % len],
                )
            });
            let Some(index) = removable else {
                break;
            };
            collapsed.remove(index);
        }
    }
    collapsed
}

fn collinear_middle_is_redundant(start: (f32, f32), middle: (f32, f32), end: (f32, f32)) -> bool {
    let baseline = point_distance_squared(start, end).sqrt();
    baseline > f32::EPSILON
        && distance_to_segment(middle, start, end) <= baseline * COLLINEAR_RELATIVE_TOLERANCE
}

#[derive(Debug, Clone, Copy)]
struct CurveParameters {
    corner_fraction: f32,
    sample_spacing: f32,
    minimum_turn: f32,
    jitter_tolerance: f32,
}

fn curve_parameters(level: usize) -> CurveParameters {
    match level.min(3) {
        1 => CurveParameters {
            corner_fraction: 0.2,
            sample_spacing: 8.0,
            minimum_turn: 0.03,
            jitter_tolerance: 0.45,
        },
        2 => CurveParameters {
            corner_fraction: 0.35,
            sample_spacing: 4.0,
            minimum_turn: 0.008,
            jitter_tolerance: 0.9,
        },
        _ => CurveParameters {
            corner_fraction: 0.5,
            sample_spacing: 2.0,
            minimum_turn: 0.002,
            jitter_tolerance: 1.5,
        },
    }
}

fn prefilter_smoothing_points(
    points: &[(f32, f32)],
    closed: bool,
    tolerance: f32,
) -> Vec<(f32, f32)> {
    if points.len() <= 2 || points.len() > MAX_JITTER_PREFILTER_POINTS {
        return points.to_vec();
    }
    if !closed {
        return simplify_open_rdp(points, tolerance);
    }

    let split = (1..points.len())
        .max_by(|left, right| {
            point_distance_squared(points[0], points[*left])
                .total_cmp(&point_distance_squared(points[0], points[*right]))
        })
        .unwrap_or(0);
    if split == 0 {
        return points.to_vec();
    }

    let first_arc = simplify_open_rdp(&points[..=split], tolerance);
    let mut second_source = Vec::with_capacity(points.len() - split + 1);
    second_source.extend_from_slice(&points[split..]);
    second_source.push(points[0]);
    let second_arc = simplify_open_rdp(&second_source, tolerance);

    let mut simplified = first_arc;
    simplified.extend(second_arc.into_iter().skip(1));
    if simplified.last() == simplified.first() {
        simplified.pop();
    }
    if simplified.len() < 3 {
        points.to_vec()
    } else {
        simplified
    }
}

fn simplify_open_rdp(points: &[(f32, f32)], tolerance: f32) -> Vec<(f32, f32)> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let mut simplified = Vec::with_capacity(points.len());
    simplified.push(points[0]);
    let mut start = 0;
    while start < points.len() - 1 {
        let mut end = start + 1;
        let mut path_length = point_distance_squared(points[start], points[end]).sqrt();
        while end < points.len() - 1 && end - start < JITTER_PREFILTER_CHUNK_POINTS {
            let next_length = point_distance_squared(points[end], points[end + 1]).sqrt();
            if path_length + next_length > JITTER_PREFILTER_CHUNK_LENGTH_PX {
                break;
            }
            path_length += next_length;
            end += 1;
        }
        simplified.extend(
            simplify_open_rdp_chunk(&points[start..=end], tolerance)
                .into_iter()
                .skip(1),
        );
        start = end;
    }
    simplified
}

fn simplify_open_rdp_chunk(points: &[(f32, f32)], tolerance: f32) -> Vec<(f32, f32)> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut pending = vec![(0, points.len() - 1)];
    while let Some((start, end)) = pending.pop() {
        if end <= start + 1 {
            continue;
        }
        let mut farthest = None;
        let mut farthest_distance = tolerance;
        for index in start + 1..end {
            let distance = distance_to_segment(points[index], points[start], points[end]);
            if distance > farthest_distance {
                farthest = Some(index);
                farthest_distance = distance;
            }
        }
        if let Some(index) = farthest {
            keep[index] = true;
            pending.push((start, index));
            pending.push((index, end));
        }
    }

    points
        .iter()
        .zip(keep)
        .filter_map(|(point, keep)| keep.then_some(*point))
        .collect()
}

fn append_smoothed_corner(
    output: &mut Vec<(f32, f32)>,
    previous: (f32, f32),
    corner: (f32, f32),
    next: (f32, f32),
    parameters: CurveParameters,
) {
    if turn_amount(previous, corner, next) < parameters.minimum_turn {
        push_unique(output, corner);
        return;
    }

    let enter = lerp(corner, previous, parameters.corner_fraction);
    let exit = lerp(corner, next, parameters.corner_fraction);
    push_unique(output, enter);
    let approximate_length =
        point_distance_squared(enter, corner).sqrt() + point_distance_squared(corner, exit).sqrt();
    let steps = ((approximate_length / parameters.sample_spacing).ceil() as usize).clamp(2, 16);
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        push_unique(output, quadratic_point(enter, corner, exit, t));
    }
}

fn append_stable_smoothed_corner(
    output: &mut Vec<(f32, f32)>,
    previous: (f32, f32),
    corner: (f32, f32),
    next: (f32, f32),
    parameters: CurveParameters,
    passes: usize,
) {
    if turn_amount(previous, corner, next) < parameters.minimum_turn {
        push_unique(output, corner);
        return;
    }

    let enter = lerp(corner, previous, parameters.corner_fraction);
    let exit = lerp(corner, next, parameters.corner_fraction);
    push_unique(output, enter);
    let steps = stable_corner_steps(passes);
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        push_unique(output, quadratic_point(enter, corner, exit, t));
    }
}

fn stable_corner_steps(passes: usize) -> usize {
    match passes.min(3) {
        1 => 2,
        2 => 4,
        _ => 8,
    }
}

fn turn_amount(previous: (f32, f32), corner: (f32, f32), next: (f32, f32)) -> f32 {
    let incoming = (corner.0 - previous.0, corner.1 - previous.1);
    let outgoing = (next.0 - corner.0, next.1 - corner.1);
    let incoming_length = (incoming.0 * incoming.0 + incoming.1 * incoming.1).sqrt();
    let outgoing_length = (outgoing.0 * outgoing.0 + outgoing.1 * outgoing.1).sqrt();
    if incoming_length <= f32::EPSILON || outgoing_length <= f32::EPSILON {
        return 0.0;
    }
    let cosine = ((incoming.0 * outgoing.0 + incoming.1 * outgoing.1)
        / (incoming_length * outgoing_length))
        .clamp(-1.0, 1.0);
    1.0 - cosine
}

fn quadratic_point(start: (f32, f32), control: (f32, f32), end: (f32, f32), t: f32) -> (f32, f32) {
    let inverse = 1.0 - t;
    (
        inverse * inverse * start.0 + 2.0 * inverse * t * control.0 + t * t * end.0,
        inverse * inverse * start.1 + 2.0 * inverse * t * control.1 + t * t * end.1,
    )
}

fn lerp(start: (f32, f32), end: (f32, f32), t: f32) -> (f32, f32) {
    (
        start.0 + (end.0 - start.0) * t,
        start.1 + (end.1 - start.1) * t,
    )
}

fn push_unique(output: &mut Vec<(f32, f32)>, point: (f32, f32)) {
    if output
        .last()
        .is_none_or(|last| !points_are_close(*last, point))
    {
        output.push(point);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GeometryClipRect, STROKE_SMOOTHING_PASSES, clip_polygon_to_rect, clip_polyline_to_rect,
        distance_to_segment, prefilter_smoothing_points, simplify_render_points,
        smooth_closed_points, smooth_closed_points_stable, smooth_stroke_points,
        smooth_stroke_points_stable,
    };

    #[test]
    fn smoothing_keeps_endpoints_and_adds_curve_points() {
        let points = [(0.0, 0.0), (10.0, 10.0), (20.0, 0.0)];
        let smoothed = smooth_stroke_points(&points, STROKE_SMOOTHING_PASSES);

        assert_eq!(smoothed.first(), Some(&points[0]));
        assert_eq!(smoothed.last(), Some(&points[2]));
        assert!(smoothed.len() > points.len());
        assert!(!smoothed.contains(&(10.0, 10.0)));
        assert!(smoothed.iter().any(|point| point.1 < 10.0 && point.1 > 0.0));
    }

    #[test]
    fn smoothing_off_returns_exact_raw_points() {
        let points = [(0.0, 0.0), (10.0, 10.0), (20.0, 0.0)];

        assert_eq!(smooth_stroke_points(&points, 0), points);
        assert_eq!(smooth_closed_points(&points, 0), points);
    }

    #[test]
    fn smoothing_leaves_two_point_strokes_unchanged() {
        let points = [(0.0, 0.0), (10.0, 0.0)];

        assert_eq!(
            smooth_stroke_points(&points, STROKE_SMOOTHING_PASSES),
            points
        );
    }

    #[test]
    fn stable_smoothing_scales_without_changing_shape() {
        let points = [(0.0, 0.0), (10.0, 30.0), (20.0, -5.0), (32.0, 12.0)];
        let scaled: Vec<_> = points.iter().map(|(x, y)| (x * 7.5, y * 7.5)).collect();

        let smoothed = smooth_stroke_points_stable(&points, 3);
        let scaled_smoothed = smooth_stroke_points_stable(&scaled, 3);

        assert_eq!(smoothed.len(), scaled_smoothed.len());
        for ((x, y), (scaled_x, scaled_y)) in smoothed.iter().zip(scaled_smoothed) {
            assert!((x * 7.5 - scaled_x).abs() < 0.001);
            assert!((y * 7.5 - scaled_y).abs() < 0.001);
        }
    }

    #[test]
    fn stable_closed_smoothing_scales_without_changing_shape() {
        let points = [(0.0, 0.0), (20.0, 0.0), (12.0, 18.0), (0.0, 0.0)];
        let scaled: Vec<_> = points.iter().map(|(x, y)| (x * 4.0, y * 4.0)).collect();

        let smoothed = smooth_closed_points_stable(&points, 2);
        let scaled_smoothed = smooth_closed_points_stable(&scaled, 2);

        assert_eq!(smoothed.len(), scaled_smoothed.len());
        for ((x, y), (scaled_x, scaled_y)) in smoothed.iter().zip(scaled_smoothed) {
            assert!((x * 4.0 - scaled_x).abs() < 0.001);
            assert!((y * 4.0 - scaled_y).abs() < 0.001);
        }
    }

    #[test]
    fn stable_smoothing_ignores_linear_stall_interpolation() {
        let sparse = [(0.0, 0.0), (16.0, 16.0), (32.0, 0.0)];
        let interpolated = [
            (0.0, 0.0),
            (4.0, 4.0),
            (8.0, 8.0),
            (12.0, 12.0),
            (16.0, 16.0),
            (20.0, 12.0),
            (24.0, 8.0),
            (28.0, 4.0),
            (32.0, 0.0),
        ];
        assert_eq!(
            smooth_stroke_points_stable(&interpolated, 2),
            smooth_stroke_points_stable(&sparse, 2)
        );

        let sparse_ring = [
            (0.0, 0.0),
            (16.0, 0.0),
            (16.0, 16.0),
            (0.0, 16.0),
            (0.0, 0.0),
        ];
        let interpolated_ring = [
            (0.0, 0.0),
            (8.0, 0.0),
            (16.0, 0.0),
            (16.0, 8.0),
            (16.0, 16.0),
            (8.0, 16.0),
            (0.0, 16.0),
            (0.0, 8.0),
            (0.0, 0.0),
        ];
        assert_eq!(
            smooth_closed_points_stable(&interpolated_ring, 2),
            smooth_closed_points_stable(&sparse_ring, 2)
        );
    }

    #[test]
    fn closed_smoothing_rounds_every_corner_without_an_open_seam() {
        let open_ring = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let closed_ring = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ];

        let open_result = smooth_closed_points(&open_ring, STROKE_SMOOTHING_PASSES);
        let closed_result = smooth_closed_points(&closed_ring, STROKE_SMOOTHING_PASSES);

        assert_eq!(open_result, closed_result);
        assert!(open_result.len() > open_ring.len());
        assert!(open_result.len() <= open_ring.len() * 18);
        assert!(open_ring.iter().all(|corner| !open_result.contains(corner)));
    }

    #[test]
    fn dense_gentle_stylus_curve_is_not_overprocessed() {
        let points: Vec<_> = (0..100)
            .map(|index| {
                let x = index as f32;
                (x, x * x * 0.001)
            })
            .collect();
        let smoothed = smooth_stroke_points(&points, STROKE_SMOOTHING_PASSES);

        assert_eq!(smoothed.first(), points.first());
        assert_eq!(smoothed.last(), points.last());
        assert!(smoothed.len() < points.len());
        assert!(
            points
                .iter()
                .all(|point| distance_to_polyline(*point, &smoothed) <= 0.91)
        );
    }

    #[test]
    fn smoothing_removes_integer_diagonal_staircase() {
        let mut points = Vec::new();
        for coordinate in 0..20 {
            points.push((coordinate as f32, coordinate as f32));
            points.push(((coordinate + 1) as f32, coordinate as f32));
        }
        points.push((20.0, 20.0));

        let smoothed = smooth_stroke_points(&points, 3);

        assert_eq!(smoothed.first(), Some(&(0.0, 0.0)));
        assert_eq!(smoothed.last(), Some(&(20.0, 20.0)));
        assert!(smoothed.len() <= 3);
        assert!(
            smoothed
                .iter()
                .all(|point| (point.0 - point.1).abs() <= f32::EPSILON)
        );
    }

    #[test]
    fn closed_jitter_prefilter_preserves_a_valid_ring() {
        let points = [
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (3.0, 0.0),
            (3.0, 1.0),
            (3.0, 2.0),
            (3.0, 3.0),
            (2.0, 3.0),
            (1.0, 3.0),
            (0.0, 3.0),
            (0.0, 2.0),
            (0.0, 1.0),
        ];
        let filtered = prefilter_smoothing_points(&points, true, 0.9);

        assert!(filtered.len() >= 4);
        assert!(filtered.len() < points.len());
        assert!(filtered.contains(&(0.0, 0.0)));
        assert!(filtered.contains(&(3.0, 3.0)));
    }

    #[test]
    fn strong_sparse_curve_output_is_bounded() {
        let points: Vec<_> = (0..100)
            .map(|index| (index as f32 * 20.0, (index % 2) as f32 * 20.0))
            .collect();
        let smoothed = smooth_stroke_points(&points, 3);

        assert_eq!(smoothed.first(), points.first());
        assert_eq!(smoothed.last(), points.last());
        assert!(smoothed.len() <= points.len() * 18);
    }

    #[test]
    fn sparse_render_geometry_is_not_modified() {
        let points = vec![(0.0, 0.0), (5.0, 2.0), (10.0, 0.0)];

        assert_eq!(simplify_render_points(&points, false), points);
    }

    #[test]
    fn dense_near_collinear_geometry_preserves_endpoints() {
        let points: Vec<_> = (0..1000)
            .map(|index| (index as f32 * 0.1, (index % 2) as f32 * 0.01))
            .collect();
        let simplified = simplify_render_points(&points, false);

        assert_eq!(simplified.first(), points.first());
        assert_eq!(simplified.last(), points.last());
        assert!(simplified.len() < 10, "points={}", simplified.len());
    }

    #[test]
    fn crossing_polyline_is_split_and_clipped_to_rect() {
        let rect = GeometryClipRect::new(0.0, 0.0, 10.0, 10.0);
        let points = [(-5.0, 5.0), (5.0, 5.0), (15.0, 5.0), (15.0, 15.0)];
        let runs = clip_polyline_to_rect(&points, rect);

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].first(), Some(&(0.0, 5.0)));
        assert_eq!(runs[0].last(), Some(&(10.0, 5.0)));
        assert!(
            runs[0]
                .iter()
                .all(|point| point.0 >= 0.0 && point.0 <= 10.0)
        );
    }

    #[test]
    fn polygon_is_clipped_to_rect_bounds() {
        let rect = GeometryClipRect::new(0.0, 0.0, 10.0, 10.0);
        let polygon = [(-5.0, -5.0), (15.0, -5.0), (15.0, 15.0), (-5.0, 15.0)];
        let clipped = clip_polygon_to_rect(&polygon, rect);

        assert_eq!(clipped.len(), 4);
        assert!(clipped.iter().all(|point| {
            point.0 >= 0.0 && point.0 <= 10.0 && point.1 >= 0.0 && point.1 <= 10.0
        }));
    }

    fn distance_to_polyline(point: (f32, f32), polyline: &[(f32, f32)]) -> f32 {
        polyline
            .windows(2)
            .map(|segment| distance_to_segment(point, segment[0], segment[1]))
            .fold(f32::INFINITY, f32::min)
    }
}
