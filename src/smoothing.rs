pub const STROKE_SMOOTHING_PASSES: usize = 2;

pub fn smooth_stroke_points(points: &[(f32, f32)], passes: usize) -> Vec<(f32, f32)> {
    if points.len() <= 2 || passes == 0 {
        return points.to_vec();
    }

    let mut smoothed = points.to_vec();
    for _ in 0..passes {
        smoothed = chaikin_pass(&smoothed);
    }
    smoothed
}

pub fn smooth_closed_points(points: &[(f32, f32)], passes: usize) -> Vec<(f32, f32)> {
    let mut smoothed = points.to_vec();
    if smoothed.len() > 1 && smoothed.first() == smoothed.last() {
        smoothed.pop();
    }
    if smoothed.len() < 3 || passes == 0 {
        return smoothed;
    }

    for _ in 0..passes {
        let mut pass = Vec::with_capacity(smoothed.len() * 2);
        for index in 0..smoothed.len() {
            let start = smoothed[index];
            let end = smoothed[(index + 1) % smoothed.len()];
            pass.push(weighted_point(start, end, 0.75, 0.25));
            pass.push(weighted_point(start, end, 0.25, 0.75));
        }
        smoothed = pass;
    }
    smoothed
}

fn chaikin_pass(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let mut smoothed = Vec::with_capacity(points.len() * 2);
    smoothed.push(points[0]);
    for segment in points.windows(2) {
        let start = segment[0];
        let end = segment[1];
        smoothed.push(weighted_point(start, end, 0.75, 0.25));
        smoothed.push(weighted_point(start, end, 0.25, 0.75));
    }
    smoothed.push(*points.last().expect("non-empty points"));
    smoothed
}

fn weighted_point(
    start: (f32, f32),
    end: (f32, f32),
    start_weight: f32,
    end_weight: f32,
) -> (f32, f32) {
    (
        start.0 * start_weight + end.0 * end_weight,
        start.1 * start_weight + end.1 * end_weight,
    )
}

#[cfg(test)]
mod tests {
    use super::{STROKE_SMOOTHING_PASSES, smooth_closed_points, smooth_stroke_points};

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
    fn smoothing_leaves_two_point_strokes_unchanged() {
        let points = [(0.0, 0.0), (10.0, 0.0)];

        assert_eq!(
            smooth_stroke_points(&points, STROKE_SMOOTHING_PASSES),
            points
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
        assert_eq!(open_result.len(), 16);
        assert!(open_ring.iter().all(|corner| !open_result.contains(corner)));
    }
}
