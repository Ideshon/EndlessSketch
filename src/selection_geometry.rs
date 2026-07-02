const GEOMETRY_EPSILON: f64 = 1.0e-9;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

impl Point2 {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect2 {
    pub min: Point2,
    pub max: Point2,
}

impl Rect2 {
    pub fn from_points(first: Point2, second: Point2) -> Self {
        Self {
            min: Point2::new(first.x.min(second.x), first.y.min(second.y)),
            max: Point2::new(first.x.max(second.x), first.y.max(second.y)),
        }
    }

    pub fn contains(self, point: Point2) -> bool {
        point.x >= self.min.x - GEOMETRY_EPSILON
            && point.x <= self.max.x + GEOMETRY_EPSILON
            && point.y >= self.min.y - GEOMETRY_EPSILON
            && point.y <= self.max.y + GEOMETRY_EPSILON
    }

    fn vertices(self) -> [Point2; 4] {
        [
            self.min,
            Point2::new(self.max.x, self.min.y),
            self.max,
            Point2::new(self.min.x, self.max.y),
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectionShape {
    Rectangle(Rect2),
    Lasso(Vec<Point2>),
}

impl SelectionShape {
    pub fn bounds(&self) -> Option<Rect2> {
        match self {
            Self::Rectangle(rect) => Some(*rect),
            Self::Lasso(points) => bounds(points),
        }
    }

    pub fn contains_point(&self, point: Point2) -> bool {
        match self {
            Self::Rectangle(rect) => rect.contains(point),
            Self::Lasso(points) => point_in_polygon(point, points),
        }
    }

    pub fn contains_all_points(&self, points: &[Point2]) -> bool {
        !points.is_empty() && points.iter().all(|point| self.contains_point(*point))
    }

    pub fn intersects_polyline(&self, points: &[Point2], radius: f64) -> bool {
        if points.is_empty() || !self.is_valid() {
            return false;
        }
        let radius = radius.max(0.0);
        if points.len() == 1 {
            return self.contains_point(points[0])
                || self
                    .edges()
                    .iter()
                    .any(|(start, end)| distance_to_segment(points[0], *start, *end) <= radius);
        }

        points
            .windows(2)
            .any(|segment| self.segment_intersects(segment[0], segment[1], radius))
    }

    pub fn intersects_polygon(&self, polygon: &[Point2]) -> bool {
        if polygon.len() < 3 || !self.is_valid() {
            return false;
        }
        if polygon.iter().any(|point| self.contains_point(*point)) {
            return true;
        }
        if self
            .vertices()
            .iter()
            .any(|point| point_in_polygon(*point, polygon))
        {
            return true;
        }

        let selection_edges = self.edges();
        polygon_edges(polygon)
            .iter()
            .any(|(polygon_start, polygon_end)| {
                selection_edges
                    .iter()
                    .any(|(selection_start, selection_end)| {
                        segments_intersect(
                            *polygon_start,
                            *polygon_end,
                            *selection_start,
                            *selection_end,
                        )
                    })
            })
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::Rectangle(_) => true,
            Self::Lasso(points) => points.len() >= 3,
        }
    }

    fn vertices(&self) -> Vec<Point2> {
        match self {
            Self::Rectangle(rect) => rect.vertices().to_vec(),
            Self::Lasso(points) => points.clone(),
        }
    }

    fn edges(&self) -> Vec<(Point2, Point2)> {
        match self {
            Self::Rectangle(rect) => polygon_edges(&rect.vertices()),
            Self::Lasso(points) => polygon_edges(points),
        }
    }

    fn segment_intersects(&self, start: Point2, end: Point2, radius: f64) -> bool {
        if self.contains_point(start) || self.contains_point(end) {
            return true;
        }
        self.edges().iter().any(|(edge_start, edge_end)| {
            segments_intersect(start, end, *edge_start, *edge_end)
                || distance_to_segment(*edge_start, start, end) <= radius
                || distance_to_segment(*edge_end, start, end) <= radius
                || distance_to_segment(start, *edge_start, *edge_end) <= radius
                || distance_to_segment(end, *edge_start, *edge_end) <= radius
        })
    }
}

fn bounds(points: &[Point2]) -> Option<Rect2> {
    let first = *points.first()?;
    let mut min = first;
    let mut max = first;
    for point in &points[1..] {
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
    }
    Some(Rect2 { min, max })
}

fn polygon_edges(points: &[Point2]) -> Vec<(Point2, Point2)> {
    if points.len() < 2 {
        return Vec::new();
    }
    let mut edges: Vec<_> = points
        .windows(2)
        .map(|window| (window[0], window[1]))
        .collect();
    edges.push((*points.last().unwrap(), points[0]));
    edges
}

fn point_in_polygon(point: Point2, polygon: &[Point2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    if polygon_edges(polygon)
        .iter()
        .any(|(start, end)| distance_to_segment(point, *start, *end) <= GEOMETRY_EPSILON)
    {
        return true;
    }

    let mut inside = false;
    let mut previous = *polygon.last().unwrap();
    for current in polygon {
        let crosses = (current.y > point.y) != (previous.y > point.y);
        if crosses {
            let x = (previous.x - current.x) * (point.y - current.y) / (previous.y - current.y)
                + current.x;
            if point.x < x {
                inside = !inside;
            }
        }
        previous = *current;
    }
    inside
}

fn segments_intersect(
    first_start: Point2,
    first_end: Point2,
    second_start: Point2,
    second_end: Point2,
) -> bool {
    let first_a = orientation(first_start, first_end, second_start);
    let first_b = orientation(first_start, first_end, second_end);
    let second_a = orientation(second_start, second_end, first_start);
    let second_b = orientation(second_start, second_end, first_end);

    if first_a * first_b < -GEOMETRY_EPSILON && second_a * second_b < -GEOMETRY_EPSILON {
        return true;
    }
    (first_a.abs() <= GEOMETRY_EPSILON && point_on_segment(second_start, first_start, first_end))
        || (first_b.abs() <= GEOMETRY_EPSILON
            && point_on_segment(second_end, first_start, first_end))
        || (second_a.abs() <= GEOMETRY_EPSILON
            && point_on_segment(first_start, second_start, second_end))
        || (second_b.abs() <= GEOMETRY_EPSILON
            && point_on_segment(first_end, second_start, second_end))
}

fn orientation(start: Point2, end: Point2, point: Point2) -> f64 {
    (end.x - start.x) * (point.y - start.y) - (end.y - start.y) * (point.x - start.x)
}

fn point_on_segment(point: Point2, start: Point2, end: Point2) -> bool {
    point.x >= start.x.min(end.x) - GEOMETRY_EPSILON
        && point.x <= start.x.max(end.x) + GEOMETRY_EPSILON
        && point.y >= start.y.min(end.y) - GEOMETRY_EPSILON
        && point.y <= start.y.max(end.y) + GEOMETRY_EPSILON
}

fn distance_to_segment(point: Point2, start: Point2, end: Point2) -> f64 {
    let delta_x = end.x - start.x;
    let delta_y = end.y - start.y;
    let length_squared = delta_x * delta_x + delta_y * delta_y;
    if length_squared <= GEOMETRY_EPSILON {
        return ((point.x - start.x).powi(2) + (point.y - start.y).powi(2)).sqrt();
    }
    let projection = (((point.x - start.x) * delta_x + (point.y - start.y) * delta_y)
        / length_squared)
        .clamp(0.0, 1.0);
    let closest = Point2::new(
        start.x + projection * delta_x,
        start.y + projection * delta_y,
    );
    ((point.x - closest.x).powi(2) + (point.y - closest.y).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::{Point2, Rect2, SelectionShape};

    fn point(x: f64, y: f64) -> Point2 {
        Point2::new(x, y)
    }

    #[test]
    fn rectangle_normalizes_drag_direction_and_contains_edges() {
        let rect = Rect2::from_points(point(10.0, 8.0), point(2.0, 4.0));

        assert_eq!(rect.min, point(2.0, 4.0));
        assert_eq!(rect.max, point(10.0, 8.0));
        assert!(rect.contains(point(2.0, 6.0)));
        assert!(!rect.contains(point(1.9, 6.0)));
    }

    #[test]
    fn rectangle_detects_crossing_stroke_without_inside_endpoints() {
        let selection =
            SelectionShape::Rectangle(Rect2::from_points(point(2.0, 2.0), point(8.0, 8.0)));
        let stroke = [point(0.0, 5.0), point(10.0, 5.0)];

        assert!(selection.intersects_polyline(&stroke, 0.0));
    }

    #[test]
    fn stroke_radius_reaches_nearby_selection_boundary() {
        let selection =
            SelectionShape::Rectangle(Rect2::from_points(point(2.0, 2.0), point(8.0, 8.0)));
        let stroke = [point(0.0, 1.5), point(10.0, 1.5)];

        assert!(!selection.intersects_polyline(&stroke, 0.4));
        assert!(selection.intersects_polyline(&stroke, 0.5));
    }

    #[test]
    fn lasso_and_polygon_intersect_when_one_contains_the_other() {
        let lasso =
            SelectionShape::Lasso(vec![point(0.0, 0.0), point(10.0, 0.0), point(5.0, 10.0)]);
        let fill = [point(4.0, 3.0), point(6.0, 3.0), point(5.0, 5.0)];

        assert!(lasso.contains_point(point(5.0, 4.0)));
        assert!(lasso.intersects_polygon(&fill));
    }

    #[test]
    fn disjoint_and_degenerate_shapes_do_not_intersect() {
        let lasso = SelectionShape::Lasso(vec![point(0.0, 0.0), point(1.0, 0.0), point(0.0, 1.0)]);
        let distant = [point(10.0, 10.0), point(12.0, 10.0)];
        let degenerate = SelectionShape::Lasso(vec![point(0.0, 0.0), point(1.0, 1.0)]);

        assert!(!lasso.intersects_polyline(&distant, 1.0));
        assert!(!degenerate.intersects_polyline(&distant, 100.0));
    }
}
