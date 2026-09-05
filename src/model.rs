use crate::coords::CameraAddress;
use crate::coords::CanvasPoint;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

pub const DEFAULT_LAYER_ID: Uuid = Uuid::from_u128(1);

fn default_layer_id() -> Uuid {
    DEFAULT_LAYER_ID
}

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
    Selection,
    EraserLasso,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditKind {
    Paint,
    Erase,
    Fill,
    EraseArea,
    CompactBlock,
}

impl EditKind {
    pub const fn is_area(self) -> bool {
        matches!(self, Self::Fill | Self::EraseArea)
    }

    pub const fn is_erase(self) -> bool {
        matches!(self, Self::Erase | Self::EraseArea)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: Uuid,
    pub name: String,
    pub camera: CameraAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    pub id: Uuid,
    pub name: String,
    pub sort_order: i64,
    pub visible: bool,
    pub locked: bool,
}

impl Layer {
    pub fn default_layer() -> Self {
        Self {
            id: DEFAULT_LAYER_ID,
            name: "Layer 1".to_owned(),
            sort_order: 0,
            visible: true,
            locked: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaintOrderUpdate {
    pub operation_id: Uuid,
    pub paint_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditOperation {
    pub id: Uuid,
    pub sequence: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_order: Option<i64>,
    #[serde(default)]
    pub transaction_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_group_id: Option<Uuid>,
    #[serde(default = "default_layer_id")]
    pub layer_id: Uuid,
    pub kind: EditKind,
    pub native_depth: i64,
    pub native_zoom: f64,
    pub points: Vec<CanvasPoint>,
    pub color: Color,
    pub width_px: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub smooth_area: bool,
    pub destructive: bool,
    pub affects_before_sequence: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstone_targets: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paint_order_updates: Vec<PaintOrderUpdate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compact_sources: Vec<EditOperation>,
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
        let destructive = kind.is_erase()
            || (matches!(kind, EditKind::Paint | EditKind::Fill) && color.is_opaque());
        let id = Uuid::new_v4();
        Self {
            id,
            sequence: 0,
            paint_order: None,
            transaction_id: id,
            fill_group_id: None,
            layer_id: DEFAULT_LAYER_ID,
            kind,
            native_depth,
            native_zoom,
            points,
            color,
            width_px,
            smooth_area: false,
            destructive,
            affects_before_sequence: None,
            tombstone_targets: Vec::new(),
            paint_order_updates: Vec::new(),
            compact_sources: Vec::new(),
        }
    }

    pub fn tombstone(mut targets: Vec<Uuid>, layer_id: Uuid) -> Self {
        targets.sort_unstable();
        targets.dedup();
        let id = Uuid::new_v4();
        Self {
            id,
            sequence: 0,
            paint_order: None,
            transaction_id: id,
            fill_group_id: None,
            layer_id,
            kind: EditKind::Erase,
            native_depth: 0,
            native_zoom: 1.0,
            points: Vec::new(),
            color: Color::WHITE,
            width_px: 0.0,
            smooth_area: false,
            destructive: true,
            affects_before_sequence: None,
            tombstone_targets: targets,
            paint_order_updates: Vec::new(),
            compact_sources: Vec::new(),
        }
    }

    pub fn paint_order(updates: Vec<PaintOrderUpdate>, layer_id: Uuid) -> Self {
        let id = Uuid::new_v4();
        Self {
            id,
            sequence: 0,
            paint_order: None,
            transaction_id: id,
            fill_group_id: None,
            layer_id,
            kind: EditKind::Paint,
            native_depth: 0,
            native_zoom: 1.0,
            points: Vec::new(),
            color: Color::WHITE,
            width_px: 0.0,
            smooth_area: false,
            destructive: false,
            affects_before_sequence: None,
            tombstone_targets: Vec::new(),
            paint_order_updates: updates,
            compact_sources: Vec::new(),
        }
    }

    pub fn compact_block(
        layer_id: Uuid,
        points: Vec<CanvasPoint>,
        sources: Vec<EditOperation>,
    ) -> Self {
        let id = Uuid::new_v4();
        Self {
            id,
            sequence: 0,
            paint_order: None,
            transaction_id: id,
            fill_group_id: None,
            layer_id,
            kind: EditKind::CompactBlock,
            native_depth: points.first().map_or(0, |point| point.depth),
            native_zoom: 1.0,
            points,
            color: Color::BLACK,
            width_px: 0.0,
            smooth_area: false,
            destructive: false,
            affects_before_sequence: None,
            tombstone_targets: Vec::new(),
            paint_order_updates: Vec::new(),
            compact_sources: sources,
        }
    }

    pub fn normalize_metadata(&mut self) {
        if self.transaction_id.is_nil() {
            self.transaction_id = self.id;
        }
        if self.layer_id.is_nil() {
            self.layer_id = DEFAULT_LAYER_ID;
        }
        if self.kind != EditKind::Fill || self.fill_group_id.is_some_and(|id| id.is_nil()) {
            self.fill_group_id = None;
        }
        if self.kind != EditKind::Fill {
            self.smooth_area = false;
        }
        self.tombstone_targets
            .retain(|target_id| *target_id != self.id);
        self.tombstone_targets.sort_unstable();
        self.tombstone_targets.dedup();
        if self.is_compact_block() {
            self.compact_sources
                .retain(|source| source.id != self.id && !source.is_metadata_command());
            for source in &mut self.compact_sources {
                source.normalize_metadata();
            }
        } else {
            self.compact_sources.clear();
        }
    }

    pub fn is_tombstone(&self) -> bool {
        !self.tombstone_targets.is_empty()
    }

    pub fn is_paint_order_command(&self) -> bool {
        !self.paint_order_updates.is_empty()
    }

    pub fn is_compact_block(&self) -> bool {
        matches!(self.kind, EditKind::CompactBlock)
    }

    pub fn is_metadata_command(&self) -> bool {
        self.is_tombstone() || self.is_paint_order_command()
    }

    pub fn effective_paint_order(&self) -> i64 {
        self.paint_order.unwrap_or(self.sequence)
    }

    pub fn object_group_id(&self) -> Uuid {
        self.fill_group_id.unwrap_or(self.id)
    }

    pub fn shares_fill_group_with(&self, other: &Self) -> bool {
        self.kind == EditKind::Fill
            && other.kind == EditKind::Fill
            && self.fill_group_id.is_some()
            && self.fill_group_id == other.fill_group_id
            && self.layer_id == other.layer_id
            && self.color == other.color
            && self.effective_paint_order() == other.effective_paint_order()
    }

    pub fn visible_color(&self, background: Color) -> Color {
        if self.kind.is_erase() {
            background
        } else {
            self.color
        }
    }

    pub fn opaque_visible_color(&self, background: Color) -> Color {
        self.visible_color(background).flattened_over(background)
    }
}

pub(crate) fn remap_fill_group_ids(operations: &mut [EditOperation]) {
    fn remap(operation: &mut EditOperation, ids: &mut HashMap<Uuid, Uuid>) {
        if let Some(group_id) = operation.fill_group_id {
            operation.fill_group_id = Some(*ids.entry(group_id).or_insert_with(Uuid::new_v4));
        }
        for source in &mut operation.compact_sources {
            remap(source, ids);
        }
    }

    let mut ids = HashMap::new();
    for operation in operations {
        remap(operation, &mut ids);
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn set_operation_layer_recursive(operation: &mut EditOperation, layer_id: Uuid) {
    operation.layer_id = layer_id;
    for source in &mut operation.compact_sources {
        set_operation_layer_recursive(source, layer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, DEFAULT_LAYER_ID, EditKind, EditOperation, Layer};
    use crate::coords::CanvasPoint;
    use num_bigint::BigInt;
    use uuid::Uuid;

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

    #[test]
    fn new_operations_have_single_operation_transaction_and_default_layer() {
        let operation =
            EditOperation::draft(EditKind::Paint, 0, 1.0, Vec::new(), Color::BLACK, 8.0);

        assert_eq!(operation.transaction_id, operation.id);
        assert_eq!(operation.layer_id, DEFAULT_LAYER_ID);
        assert_eq!(Layer::default_layer().id, DEFAULT_LAYER_ID);
    }

    #[test]
    fn legacy_metadata_is_normalized_without_changing_operation_identity() {
        let mut operation =
            EditOperation::draft(EditKind::Paint, 0, 1.0, Vec::new(), Color::BLACK, 8.0);
        let id = operation.id;
        operation.transaction_id = Uuid::nil();
        operation.layer_id = Uuid::nil();

        operation.normalize_metadata();

        assert_eq!(operation.id, id);
        assert_eq!(operation.transaction_id, id);
        assert_eq!(operation.layer_id, DEFAULT_LAYER_ID);
    }

    #[test]
    fn fill_group_identity_round_trips_and_legacy_payload_defaults_to_none() {
        let mut operation =
            EditOperation::draft(EditKind::Fill, 0, 1.0, Vec::new(), Color::BLACK, 0.0);
        let group_id = Uuid::new_v4();
        operation.fill_group_id = Some(group_id);
        operation.smooth_area = true;

        let mut payload = serde_json::to_value(&operation).unwrap();
        let decoded: EditOperation = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(decoded.fill_group_id, Some(group_id));
        assert!(decoded.smooth_area);
        assert_eq!(decoded.object_group_id(), group_id);

        payload.as_object_mut().unwrap().remove("fill_group_id");
        payload.as_object_mut().unwrap().remove("smooth_area");
        let legacy: EditOperation = serde_json::from_value(payload).unwrap();
        assert_eq!(legacy.fill_group_id, None);
        assert!(!legacy.smooth_area);
        assert_eq!(legacy.object_group_id(), legacy.id);

        let mut invalid = operation;
        invalid.kind = EditKind::Paint;
        invalid.normalize_metadata();
        assert_eq!(invalid.fill_group_id, None);
    }
}
