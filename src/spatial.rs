use crate::coords::DEPTH_RATIO;
use crate::model::EditOperation;
use crate::tile_cache::TileKey;
use num_bigint::BigInt;
use num_traits::{Euclid, ToPrimitive};
use std::collections::{BTreeMap, HashMap, HashSet};

const MAX_INDEXED_TILES_PER_OPERATION: u64 = 4096;

#[derive(Default)]
pub struct OperationIndex {
    depths: HashMap<i64, DepthBucket>,
    broad_operations: Vec<usize>,
}

#[derive(Default)]
struct DepthBucket {
    columns: BTreeMap<BigInt, BTreeMap<BigInt, Vec<usize>>>,
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
        let Some(bounds) = operation_bounds(operation) else {
            return;
        };
        if !bounds.indexable_within(MAX_INDEXED_TILES_PER_OPERATION) {
            self.broad_operations.push(operation_index);
            return;
        }

        let mut x = bounds.min_x;
        let one = BigInt::from(1u8);
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
                y += &one;
            }
            x += &one;
        }
    }

    pub fn query(&self, tile: &TileKey) -> Vec<usize> {
        self.query_many(std::slice::from_ref(tile))
    }

    pub fn query_many(&self, tiles: &[TileKey]) -> Vec<usize> {
        let mut matches: HashSet<_> = self.broad_operations.iter().copied().collect();
        for tile in tiles {
            for (&source_depth, bucket) in &self.depths {
                if source_depth <= tile.depth {
                    query_coarse_or_equal_depth(tile, source_depth, bucket, &mut matches);
                } else {
                    query_finer_depth(tile, source_depth, bucket, &mut matches);
                }
            }
        }
        let mut matches: Vec<_> = matches.into_iter().collect();
        matches.sort_unstable();
        matches
    }
}

fn query_coarse_or_equal_depth(
    tile: &TileKey,
    source_depth: i64,
    bucket: &DepthBucket,
    matches: &mut HashSet<usize>,
) {
    let levels = tile.depth.saturating_sub(source_depth);
    let Some(factor) = depth_factor(levels) else {
        return;
    };
    let source_x = tile.x.div_euclid(&factor);
    let source_y = tile.y.div_euclid(&factor);
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
    source_depth: i64,
    bucket: &DepthBucket,
    matches: &mut HashSet<usize>,
) {
    let levels = source_depth.saturating_sub(tile.depth);
    let Some(factor) = depth_factor(levels) else {
        return;
    };
    let min_x: BigInt = &tile.x * &factor - 1;
    let max_x: BigInt = (&tile.x + 1) * &factor;
    let min_y: BigInt = &tile.y * &factor - 1;
    let max_y: BigInt = (&tile.y + 1) * &factor;
    for (_, column) in bucket.columns.range(min_x..=max_x) {
        for (_, indices) in column.range(min_y.clone()..=max_y.clone()) {
            matches.extend(indices.iter().copied());
        }
    }
}

fn depth_factor(levels: i64) -> Option<BigInt> {
    if !(0..=u32::MAX as i64).contains(&levels) {
        return None;
    }
    Some(BigInt::from(DEPTH_RATIO).pow(levels as u32))
}

#[derive(Debug)]
struct OperationBounds {
    depth: i64,
    min_x: BigInt,
    max_x: BigInt,
    min_y: BigInt,
    max_y: BigInt,
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

fn operation_bounds(operation: &EditOperation) -> Option<OperationBounds> {
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
}
