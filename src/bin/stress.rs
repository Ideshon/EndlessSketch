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
    println!("generating {operation_count} operations across 12 depth bands");

    let generation_started = Instant::now();
    let operations: Vec<_> = (0..operation_count)
        .map(|index| synthetic_operation(index as u64))
        .collect();
    let generation_time = generation_started.elapsed();

    let indexing_started = Instant::now();
    let index = OperationIndex::build(&operations);
    let indexing_time = indexing_started.elapsed();

    let query_started = Instant::now();
    let mut total_matches = 0usize;
    for y in -3..=3 {
        for x in -3..=3 {
            total_matches += index
                .query(&TileKey {
                    depth: 0,
                    x: BigInt::from(x),
                    y: BigInt::from(y),
                    lod: 0,
                })
                .len();
        }
    }
    let query_time = query_started.elapsed();

    println!("generation: {generation_time:.2?}");
    println!("indexing:   {indexing_time:.2?}");
    println!("49 queries: {query_time:.2?} ({total_matches} total matches)");
}

fn synthetic_operation(index: u64) -> EditOperation {
    let depth = (index % 12) as i64;
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
