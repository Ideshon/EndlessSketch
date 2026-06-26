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
}
