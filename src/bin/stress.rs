use endless_sketch::coords::CanvasPoint;
use endless_sketch::model::{Color, EditKind, EditOperation};
use endless_sketch::spatial::OperationIndex;
use endless_sketch::tile_cache::TileKey;
use num_bigint::BigInt;
use std::time::Instant;

fn main() {
    let operation_count = std::env::args()
        .nth(1)
        .and_then(|argument| argument.parse::<usize>().ok())
        .unwrap_or(1_000_000);
    let depth_radius = std::env::args()
        .nth(2)
        .and_then(|argument| argument.parse::<i64>().ok())
        .unwrap_or(6)
        .clamp(0, 10_000);
    let depth_band_count = depth_radius.saturating_mul(2).saturating_add(1);
    println!(
        "generating {operation_count} operations across {depth_band_count} depth bands (-{depth_radius}..+{depth_radius})"
    );

    let generation_started = Instant::now();
    let operations: Vec<_> = (0..operation_count)
        .map(|index| synthetic_operation(index as u64, depth_radius))
        .collect();
    let generation_time = generation_started.elapsed();

    let indexing_started = Instant::now();
    let index = OperationIndex::build(&operations);
    let indexing_time = indexing_started.elapsed();

    let query_started = Instant::now();
    let mut total_matches = 0usize;
    let query_groups: Vec<Vec<_>> = [-depth_radius, 0, depth_radius]
        .into_iter()
        .map(|depth| {
            let mut keys = Vec::with_capacity(49);
            for y in -3..=3 {
                for x in -3..=3 {
                    keys.push(TileKey {
                        depth,
                        x: BigInt::from(x),
                        y: BigInt::from(y),
                        lod: 0,
                    });
                }
            }
            keys
        })
        .collect();
    for keys in &query_groups {
        for key in keys {
            total_matches += index.query(key).len();
        }
    }
    let query_time = query_started.elapsed();

    let batched_query_started = Instant::now();
    let mut batched_total_matches = 0usize;
    for keys in &query_groups {
        batched_total_matches += index.query_many(keys).len();
    }
    let batched_query_time = batched_query_started.elapsed();

    println!("generation:          {generation_time:.2?}");
    println!("indexing:            {indexing_time:.2?}");
    println!("147 tile queries:    {query_time:.2?} ({total_matches} total matches)");
    println!(
        "3 viewport queries:  {batched_query_time:.2?} ({batched_total_matches} unique matches)"
    );
}

fn synthetic_operation(index: u64, depth_radius: i64) -> EditOperation {
    let depth_band_count = depth_radius.saturating_mul(2).saturating_add(1) as u64;
    let depth = (index % depth_band_count) as i64 - depth_radius;
    let pseudo_x = mix(index) as i64 % 20_000 - 10_000;
    let pseudo_y = mix(index ^ 0x9e37_79b9_7f4a_7c15) as i64 % 20_000 - 10_000;
    EditOperation::draft(
        EditKind::Paint,
        depth,
        1.0,
        vec![
            CanvasPoint::new(
                depth,
                BigInt::from(pseudo_x),
                BigInt::from(pseudo_y),
                0.1,
                0.2,
            ),
            CanvasPoint::new(
                depth,
                BigInt::from(pseudo_x),
                BigInt::from(pseudo_y),
                0.2,
                0.3,
            ),
        ],
        Color::BLACK,
        5.0,
    )
}

fn mix(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
