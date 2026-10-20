# tilemaker-pbf

A fast, streaming OpenStreetMap PBF reader (and a simple writer).

```rust,no_run
use tilemaker_pbf::{DecodeOptions, PbfFile};

let file = PbfFile::open("planet.osm.pbf")?;      // indexes blob headers only
for block in file.blocks(0..file.blobs().len(), DecodeOptions::ALL, 8) {
    let (_, block) = block?;                       // in file order, decoded on 8 threads
    for node in block.nodes() {
        if let Some(name) = node.tags().get("name") { /* … */ }
    }
}
# Ok::<(), tilemaker_pbf::Error>(())
```

- Parallel, order-preserving decoding with a bounded window
  (`PbfFile::blocks`, `map_blocks`, `map_block_list`): ~1.1 GB/s on 10
  cores (the Netherlands' 2.6 GB in 2.4 s).
- `BlobReader` reads blobs sequentially from any `Read`.
- Dense and plain nodes, ways (with `LocationsOnWays`), relations,
  metadata; raw, zlib (libdeflate), zstd and lz4 blobs; zero-copy tag views
  over the block's string table (one buffer per block).
- `PbfFile::layout()` finds the node/way/relation block ranges of sorted
  files with a binary search; `first_element` reads a block's first id by
  inflating only its start, and `blocks_with_ids` skips blocks that cannot
  hold wanted ids.
- `PbfWriter` writes PBF files; `BlockBuilder` and `ParallelWriter` build
  blocks anywhere and encode and compress them on worker threads.
- Malformed input is an error, never a panic.
