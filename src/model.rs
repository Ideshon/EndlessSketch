use crate::coords::CameraAddress;
use crate::coords::CanvasPoint;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Self = Self::rgba(20, 20, 24, 255);
    pub const WHITE: Self = Self::rgba(250, 250, 248, 255);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn is_opaque(self) -> bool {
        self.a == u8::MAX
    }

    pub fn flattened_over(self, background: Self) -> Self {
        if self.is_opaque() {
            return self;
        }
        let alpha = u32::from(self.a);
        let inverse_alpha = u32::from(u8::MAX - self.a);
        let flatten_channel = |source: u8, destination: u8| {
            ((u32::from(source) * alpha
                + u32::from(destination) * inverse_alpha
                + u32::from(u8::MAX) / 2)
                / u32::from(u8::MAX)) as u8
        };
        Self::rgba(
            flatten_channel(self.r, background.r),
            flatten_channel(self.g, background.g),
            flatten_channel(self.b, background.b),
            u8::MAX,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    Brush,
    Eraser,
    LassoFill,
    Eyedropper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditKind {
    Paint,
    Erase,
    Fill,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: Uuid,
    pub name: String,
    pub camera: CameraAddress,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditOperation {
    pub id: Uuid,
    pub sequence: i64,
    pub kind: EditKind,
    pub native_depth: i64,
    pub native_zoom: f64,
    pub points: Vec<CanvasPoint>,
    pub color: Color,
    pub width_px: f32,
    pub destructive: bool,
    pub affects_before_sequence: Option<i64>,
}

impl EditOperation {
    pub fn draft(
        kind: EditKind,
        native_depth: i64,
        native_zoom: f64,
        points: Vec<CanvasPoint>,
        color: Color,
        width_px: f32,
    ) -> Self {
        let destructive = matches!(kind, EditKind::Erase)
            || (matches!(kind, EditKind::Paint | EditKind::Fill) && color.is_opaque());
        Self {
            id: Uuid::new_v4(),
            sequence: 0,
            kind,
            native_depth,
            native_zoom,
            points,
            color,
            width_px,
            destructive,
            affects_before_sequence: None,
        }
    }

    pub fn visible_color(&self, background: Color) -> Color {
        if self.kind == EditKind::Erase {
            background
        } else {
            self.color
        }
    }

    pub fn opaque_visible_color(&self, background: Color) -> Color {
        self.visible_color(background).flattened_over(background)
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, EditKind, EditOperation};
    use crate::coords::CanvasPoint;
    use num_bigint::BigInt;

    #[test]
    fn flattening_preserves_stored_alpha_and_returns_opaque_color() {
        let source = Color::rgba(0, 0, 0, 128);
        let operation = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.25, 0.5),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.75, 0.5),
            ],
            source,
            8.0,
        );

        let flattened = operation.opaque_visible_color(Color::WHITE);

        assert_eq!(operation.color, source);
        assert_eq!(operation.color.a, 128);
        assert_eq!(flattened, Color::rgba(125, 125, 124, 255));
    }
}
