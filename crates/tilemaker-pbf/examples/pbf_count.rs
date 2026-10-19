//! Count the elements of a PBF file: `cargo run --release --example pbf_count -- file.osm.pbf [threads]`.

use std::time::Instant;

use tilemaker_pbf::{DecodeOptions, PbfFile};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: pbf_count <file.osm.pbf> [threads]")?;
    let threads = args
        .next()
        .map(|t| t.parse())
        .transpose()?
        .unwrap_or_else(tilemaker_pbf::parallel::default_threads);
    let started = Instant::now();
    let file = PbfFile::open(&path)?;
    println!("indexed {} blocks in {:.1?}", file.blobs().len(), started.elapsed());
    let (mut n, mut w, mut r) = (0u64, 0u64, 0u64);
    for b in file.blocks(0..file.blobs().len(), DecodeOptions::ALL, threads) {
        let (_, block) = b?;
        n += block.node_count() as u64;
        w += block.way_count() as u64;
        r += block.relation_count() as u64;
    }
    let secs = started.elapsed().as_secs_f64();
    println!(
        "nodes {n} ways {w} relations {r} in {secs:.1}s ({:.0} MB/s)",
        file.len() as f64 / 1e6 / secs
    );
    Ok(())
}
