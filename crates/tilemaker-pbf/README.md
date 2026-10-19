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
  (`PbfFile::blocks`, `map_blocks`, `map_block_list`); the planet decodes at
  ~600 MB/s on 10 cores.
- `BlobReader` reads blobs sequentially from any `Read`.
- Dense and plain nodes, ways (with `LocationsOnWays`), relations,
  metadata; zero-copy tag views over the block's string table.
- `PbfFile::layout()` finds the node/way/relation block ranges of sorted
  files with a binary search.
- `PbfWriter` writes zlib-compressed PBF files.
- Malformed input is an error, never a panic.
