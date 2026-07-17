use crate::coords::DEPTH_RATIO;
use crate::model::EditOperation;
use crate::tile_cache::TileKey;
use num_bigint::BigInt;
use num_traits::{Euclid, ToPrimitive};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

const MAX_INDEXED_TILES_PER_OPERATION: u64 = 4096;

#[derive(Default)]
pub struct OperationIndex {
    depths: HashMap<i64, DepthBucket>,
    broad_operations: Vec<usize>,
    operation_ids: HashMap<usize, Uuid>,
    operation_layers: HashMap<usize, Uuid>,
    operation_tiles: HashMap<usize, Vec<IndexedTile>>,
}

#[derive(Default)]
struct DepthBucket {
    columns: BTreeMap<BigInt, BTreeMap<BigInt, Vec<usize>>>,
}

#[derive(Clone)]
struct IndexedTile {
    depth: i64,
    x: BigInt,
    y: BigInt,
}

impl OperationIndex {
    pub fn build(operations: &[EditOperation]) -> Self {
        let mut index = Self::default();
        for (operation_index, operation) in operations.iter().enumerate() {
            index.insert(operation_index, operation);
        }
        index
    }

    pub fn insert(&mut self, operation_index: usize, operation: &EditOperation) {
        let bounds = operation_bounds(operation);
        self.insert_with_bounds(operation_index, operation, bounds.as_ref());
    }

    pub fn insert_with_bounds(
        &mut self,
        operation_index: usize,
        operation: &EditOperation,
        bounds: Option<&OperationBounds>,
    ) {
        self.operation_ids.insert(operation_index, operation.id);
        self.operation_layers
            .insert(operation_index, operation.layer_id);
        let Some(bounds) = bounds else {
            return;
        };
        if !bounds.indexable_within(MAX_INDEXED_TILES_PER_OPERATION) {
            self.broad_operations.push(operation_index);
            return;
        }

        let mut x = bounds.min_x.clone();
        let one = BigInt::from(1u8);
        let mut indexed_tiles = Vec::new();
        while x <= bounds.max_x {
            let mut y = bounds.min_y.clone();
            while y <= bounds.max_y {
                self.depths
                    .entry(bounds.depth)
                    .or_default()
                    .columns
                    .entry(x.clone())
                    .or_default()
                    .entry(y.clone())
                    .or_default()
                    .push(operation_index);
                indexed_tiles.push(IndexedTile {
                    depth: bounds.depth,
                    x: x.clone(),
                    y: y.clone(),
                });
                y += &one;
            }
            x += &one;
        }
        if !indexed_tiles.is_empty() {
            self.operation_tiles.insert(operation_index, indexed_tiles);
        }
    }

    pub fn query(&self, tile: &TileKey) -> Vec<usize> {
        self.query_many(std::slice::from_ref(tile))
    }

    pub fn query_many(&self, tiles: &[TileKey]) -> Vec<usize> {
        let mut matches: HashSet<_> = self.broad_operations.iter().copied().collect();
        let mut tiles_by_depth: HashMap<i64, Vec<&TileKey>> = HashMap::new();
        for tile in tiles {
            tiles_by_depth.entry(tile.depth).or_default().push(tile);
        }
        for (target_depth, target_tiles) in tiles_by_depth {
            for (&source_depth, bucket) in &self.depths {
                let levels = target_depth.abs_diff(source_depth);
                let Some(factor) = depth_factor_unsigned(levels) else {
                    continue;
                };
                for tile in &target_tiles {
                    if source_depth <= target_depth {
                        query_coarse_or_equal_depth(tile, &factor, bucket, &mut matches);
                    } else {
                        query_finer_depth(tile, &factor, bucket, &mut matches);
                    }
                }
            }
        }
        let mut matches: Vec<_> = matches.into_iter().collect();
        matches.sort_unstable();
        matches
    }

    pub fn query_many_ids(&self, tiles: &[TileKey]) -> Vec<Uuid> {
        self.query_many(tiles)
            .into_iter()
            .filter_map(|index| self.operation_ids.get(&index).copied())
            .collect()
    }

    pub fn query_many_ids_in_layers(
        &self,
        tiles: &[TileKey],
        eligible_layers: &HashSet<Uuid>,
    ) -> Vec<Uuid> {
        self.query_many(tiles)
            .into_iter()
            .filter(|index| {
                self.operation_layers
                    .get(index)
                    .is_some_and(|layer_id| eligible_layers.contains(layer_id))
            })
            .filter_map(|index| self.operation_ids.get(&index).copied())
            .collect()
    }

    pub fn remove_indices(&mut self, removed: &HashSet<usize>) {
        if removed.is_empty() {
            return;
        }
        self.broad_operations
            .retain(|index| !removed.contains(index));
        for index in removed {
            self.operation_ids.remove(index);
            self.operation_layers.remove(index);
            let Some(tiles) = self.operation_tiles.remove(index) else {
                continue;
            };
            for tile in tiles {
                let remove_bucket = {
                    let Some(bucket) = self.depths.get_mut(&tile.depth) else {
                        continue;
                    };
                    let remove_column = if let Some(column) = bucket.columns.get_mut(&tile.x) {
                        if let Some(indices) = column.get_mut(&tile.y) {
                            indices.retain(|candidate| candidate != index);
                            if indices.is_empty() {
                                column.remove(&tile.y);
                            }
                        }
                        column.is_empty()
                    } else {
                        false
                    };
                    if remove_column {
                        bucket.columns.remove(&tile.x);
                    }
                    bucket.columns.is_empty()
                };
                if remove_bucket {
                    self.depths.remove(&tile.depth);
                }
            }
        }
    }
}

fn query_coarse_or_equal_depth(
    tile: &TileKey,
    factor: &BigInt,
    bucket: &DepthBucket,
    matches: &mut HashSet<usize>,
) {
    let source_x = tile.x.div_euclid(factor);
    let source_y = tile.y.div_euclid(factor);
    for offset_x in -1..=1 {
        let x = &source_x + offset_x;
        let Some(column) = bucket.columns.get(&x) else {
            continue;
        };
        for offset_y in -1..=1 {
            if let Some(indices) = column.get(&(&source_y + offset_y)) {
                matches.extend(indices.iter().copied());
            }
        }
    }
}

fn query_finer_depth(
    tile: &TileKey,
    factor: &BigInt,
    bucket: &DepthBucket,
    matches: &mut HashSet<usize>,
) {
    let min_x: BigInt = &tile.x * factor - 1;
    let max_x: BigInt = (&tile.x + 1) * factor;
    let min_y: BigInt = &tile.y * factor - 1;
    let max_y: BigInt = (&tile.y + 1) * factor;
    for (_, column) in bucket.columns.range(min_x..=max_x) {
        for (_, indices) in column.range(min_y.clone()..=max_y.clone()) {
            matches.extend(indices.iter().copied());
        }
    }
}

fn depth_factor_unsigned(levels: u64) -> Option<BigInt> {
    if levels > u64::from(u32::MAX) {
        return None;
    }
    Some(BigInt::from(DEPTH_RATIO).pow(levels as u32))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationBounds {
    pub depth: i64,
    pub min_x: BigInt,
    pub max_x: BigInt,
    pub min_y: BigInt,
    pub max_y: BigInt,
}

impl OperationBounds {
    fn indexable_within(&self, limit: u64) -> bool {
        let width = (&self.max_x - &self.min_x + 1u8).to_u64();
        let height = (&self.max_y - &self.min_y + 1u8).to_u64();
        width
            .zip(height)
            .and_then(|(width, height)| width.checked_mul(height))
            .is_some_and(|tiles| tiles <= limit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AffectedTiles {
    Bounded(Vec<TileKey>),
    Broad,
}

pub fn operation_bounds(operation: &EditOperation) -> Option<OperationBounds> {
    let first = operation.points.first()?;
    let mut min_x = first.tile_x.clone();
    let mut max_x = first.tile_x.clone();
    let mut min_y = first.tile_y.clone();
    let mut max_y = first.tile_y.clone();

    for point in &operation.points[1..] {
        if point.tile_x < min_x {
            min_x = point.tile_x.clone();
        }
        if point.tile_x > max_x {
            max_x = point.tile_x.clone();
        }
        if point.tile_y < min_y {
            min_y = point.tile_y.clone();
        }
        if point.tile_y > max_y {
            max_y = point.tile_y.clone();
        }
    }

    Some(OperationBounds {
        depth: first.depth,
        min_x,
        max_x,
        min_y,
        max_y,
    })
}

pub fn affected_tiles(operation: &EditOperation, lod: u8) -> AffectedTiles {
    let Some(bounds) = operation_bounds(operation) else {
        return AffectedTiles::Bounded(Vec::new());
    };
    let expanded = OperationBounds {
        depth: bounds.depth,
        min_x: bounds.min_x - 1,
        max_x: bounds.max_x + 1,
        min_y: bounds.min_y - 1,
        max_y: bounds.max_y + 1,
    };
    if !expanded.indexable_within(MAX_INDEXED_TILES_PER_OPERATION) {
        return AffectedTiles::Broad;
    }

    let mut keys = Vec::new();
    let one = BigInt::from(1u8);
    let mut x = expanded.min_x;
    while x <= expanded.max_x {
        let mut y = expanded.min_y.clone();
        while y <= expanded.max_y {
            keys.push(TileKey {
                depth: expanded.depth,
                x: x.clone(),
                y: y.clone(),
                lod,
            });
            y += &one;
        }
        x += &one;
    }
    AffectedTiles::Bounded(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::CanvasPoint;
    use crate::model::{Color, EditKind};

    fn operation(depth: i64, tile_x: i64, tile_y: i64) -> EditOperation {
        EditOperation::draft(
            EditKind::Paint,
            depth,
            1.0,
            vec![
                CanvasPoint::new(depth, tile_x.into(), tile_y.into(), 0.1, 0.1),
                CanvasPoint::new(depth, tile_x.into(), tile_y.into(), 0.9, 0.9),
            ],
            Color::BLACK,
            5.0,
        )
    }

    #[test]
    fn query_finds_fine_content_from_a_coarser_tile() {
        let operations = vec![operation(6, 262_144, 262_144)];
        let index = OperationIndex::build(&operations);
        let key = TileKey {
            depth: 0,
            x: 1.into(),
            y: 1.into(),
            lod: 0,
        };
        assert_eq!(index.query(&key), vec![0]);
    }

    #[test]
    fn query_finds_coarse_content_from_a_fine_tile() {
        let operations = vec![operation(0, 1, 1)];
        let index = OperationIndex::build(&operations);
        let key = TileKey {
            depth: 6,
            x: 262_144.into(),
            y: 262_144.into(),
            lod: 0,
        };
        assert_eq!(index.query(&key), vec![0]);
    }

    #[test]
    fn query_does_not_return_distant_content() {
        let operations = vec![operation(0, 10_000, 10_000)];
        let index = OperationIndex::build(&operations);
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        assert!(index.query(&key).is_empty());
    }

    #[test]
    fn query_includes_tiles_crossed_between_sparse_points() {
        let operation = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, 0.into(), 0.into(), 0.1, 0.5),
                CanvasPoint::new(0, 2.into(), 0.into(), 0.9, 0.5),
            ],
            Color::BLACK,
            8.0,
        );
        let index = OperationIndex::build(&[operation]);
        let key = TileKey {
            depth: 0,
            x: 1.into(),
            y: 0.into(),
            lod: 0,
        };
        assert_eq!(index.query(&key), vec![0]);
    }

    #[test]
    fn multi_tile_query_deduplicates_and_excludes_distant_operations() {
        let operations = vec![
            operation(0, 0, 0),
            operation(0, 1, 0),
            operation(0, 10_000, 10_000),
        ];
        let index = OperationIndex::build(&operations);
        let visible = vec![
            TileKey {
                depth: 0,
                x: 0.into(),
                y: 0.into(),
                lod: 0,
            },
            TileKey {
                depth: 0,
                x: 1.into(),
                y: 0.into(),
                lod: 0,
            },
        ];

        assert_eq!(index.query_many(&visible), vec![0, 1]);
    }

    #[test]
    fn uuid_queries_preserve_sequence_order_and_filter_layers() {
        let mut first = operation(0, 0, 0);
        let first_id = first.id;
        first.layer_id = Uuid::from_u128(10);
        let mut second = operation(0, 1, 0);
        let second_id = second.id;
        second.layer_id = Uuid::from_u128(20);
        let operations = vec![first, second];
        let index = OperationIndex::build(&operations);
        let visible = vec![
            TileKey {
                depth: 0,
                x: 0.into(),
                y: 0.into(),
                lod: 0,
            },
            TileKey {
                depth: 0,
                x: 1.into(),
                y: 0.into(),
                lod: 0,
            },
        ];

        assert_eq!(index.query_many_ids(&visible), vec![first_id, second_id]);
        assert_eq!(
            index.query_many_ids_in_layers(&visible, &HashSet::from([Uuid::from_u128(20)])),
            vec![second_id]
        );
    }

    #[test]
    fn affected_tiles_adds_stroke_margin_and_bounds_enumeration() {
        let operation = operation(4, 10, -20);
        let AffectedTiles::Bounded(keys) = affected_tiles(&operation, 3) else {
            panic!("small operation should have bounded coverage");
        };

        assert_eq!(keys.len(), 9);
        assert!(keys.contains(&TileKey {
            depth: 4,
            x: 9.into(),
            y: (-21).into(),
            lod: 3,
        }));
        assert!(keys.contains(&TileKey {
            depth: 4,
            x: 11.into(),
            y: (-19).into(),
            lod: 3,
        }));
    }

    #[test]
    fn removing_tail_indices_preserves_remaining_uuid_queries() {
        let operations = vec![operation(0, 0, 0), operation(0, 1, 0), operation(0, 2, 0)];
        let first_id = operations[0].id;
        let mut index = OperationIndex::build(&operations);
        index.remove_indices(&HashSet::from([1, 2]));
        let keys = [
            TileKey {
                depth: 0,
                x: 0.into(),
                y: 0.into(),
                lod: 0,
            },
            TileKey {
                depth: 0,
                x: 1.into(),
                y: 0.into(),
                lod: 0,
            },
            TileKey {
                depth: 0,
                x: 2.into(),
                y: 0.into(),
                lod: 0,
            },
        ];

        assert_eq!(index.query_many_ids(&keys), vec![first_id]);
    }

    #[test]
    fn removing_middle_indices_preserves_other_tile_queries() {
        let operations = vec![
            operation(0, 0, 0),
            operation(0, 1, 0),
            operation(0, 2, 0),
            operation(0, 3, 0),
        ];
        let first_id = operations[0].id;
        let last_id = operations[3].id;
        let mut index = OperationIndex::build(&operations);
        index.remove_indices(&HashSet::from([1, 2]));
        let keys = [
            TileKey {
                depth: 0,
                x: 0.into(),
                y: 0.into(),
                lod: 0,
            },
            TileKey {
                depth: 0,
                x: 1.into(),
                y: 0.into(),
                lod: 0,
            },
            TileKey {
                depth: 0,
                x: 2.into(),
                y: 0.into(),
                lod: 0,
            },
            TileKey {
                depth: 0,
                x: 3.into(),
                y: 0.into(),
                lod: 0,
            },
        ];

        assert_eq!(index.query_many_ids(&keys), vec![first_id, last_id]);
    }

    #[test]
    fn huge_bounds_fall_back_to_broad_operation() {
        let operation = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, 0.into(), 0.into(), 0.1, 0.1),
                CanvasPoint::new(0, 10_000.into(), 10_000.into(), 0.9, 0.9),
            ],
            Color::BLACK,
            8.0,
        );
        let index = OperationIndex::build(&[operation]);
        let key = TileKey {
            depth: 0,
            x: 5000.into(),
            y: 5000.into(),
            lod: 0,
        };
        assert_eq!(index.query(&key), vec![0]);
    }

    #[test]
    fn queries_find_content_across_two_hundred_depth_levels() {
        let operations = vec![operation(-100, 0, 0), operation(100, 0, 0)];
        let index = OperationIndex::build(&operations);

        for depth in [-100, 0, 100] {
            let key = TileKey {
                depth,
                x: 0.into(),
                y: 0.into(),
                lod: 0,
            };
            assert_eq!(index.query(&key), vec![0, 1], "depth={depth}");
        }
    }

    #[test]
    fn queries_find_content_across_two_thousand_depth_levels() {
        let operations = vec![operation(-1000, 0, 0), operation(1000, 0, 0)];
        let index = OperationIndex::build(&operations);

        for depth in [-1000, 0, 1000] {
            let key = TileKey {
                depth,
                x: 0.into(),
                y: 0.into(),
                lod: 0,
            };
            assert_eq!(index.query(&key), vec![0, 1], "depth={depth}");
        }
    }

    #[test]
    fn query_finds_content_at_thousand_digit_lateral_offset() {
        let huge = BigInt::from(10u8).pow(1000);
        let operation = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, huge.clone(), -huge.clone(), 0.1, 0.1),
                CanvasPoint::new(0, huge.clone(), -huge.clone(), 0.9, 0.9),
            ],
            Color::BLACK,
            5.0,
        );
        let index = OperationIndex::build(&[operation]);
        let key = TileKey {
            depth: 0,
            x: huge.clone(),
            y: -huge,
            lod: 0,
        };

        assert_eq!(index.query(&key), vec![0]);
    }
}
