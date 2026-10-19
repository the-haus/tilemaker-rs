# tilemaker-rs

A Rust port of [tilemaker](https://github.com/systemed/tilemaker) — vector
tiles straight from OpenStreetMap data, with the same `process.lua`
profiles and `config.json` — plus a streaming OSM extractor, both usable
from the command line, from Rust, and from Node.js
([`@tilemaker-rs/node`](crates/bindings/tilemaker-node)).

```sh
# vector tiles, tilemaker-compatible arguments
tilemaker region.osm.pbf region.pmtiles --config config.json --process process.lua

# filtered, assembled features as GeoJSON lines (like osmium tags-filter + export)
tilemaker export planet.osm.pbf -f n/place=city,town -f r/admin_level=2,4 -o geography.geojsonseq

# what is in a tileset
tilemaker inspect region.pmtiles
```

```ts
import { openPbf } from "@tilemaker-rs/node";

const pbf = await openPbf("planet.osm.pbf");
for await (const f of pbf.features({ filters: ["r/admin_level=2"] })) {
  // f.geometry.coordinates is a Float64Array view; nothing is buffered
}
```

## Layout

Like [lora](https://github.com/lora-db/lora): a Cargo workspace of small
crates, and bindings under `crates/bindings`.

| crate | what |
|---|---|
| [`tilemaker-pbf`](crates/tilemaker-pbf) | OSM PBF reader (and writer): blob index, parallel order-preserving block decoding with bounded memory, sequential `BlobReader` for any `Read` |
| [`tilemaker-geom`](crates/tilemaker-geom) | projection and tile maths, clipping, simplification, ring building and multipolygon assembly, areas/lengths/centroids |
| [`tilemaker-store`](crates/tilemaker-store) | compressed id sets, node location stores (memory or dense memory-mapped file), way geometry store |
| [`tilemaker-export`](crates/tilemaker-export) | the streaming extractor: osmium-style filters, element and feature export |
| [`tilemaker-config`](crates/tilemaker-config) | `config.json` |
| [`tilemaker-lua`](crates/tilemaker-lua) | the `process.lua` API on mlua (LuaJIT, or Lua 5.4) |
| [`tilemaker-mvt`](crates/tilemaker-mvt) | Mapbox Vector Tile encoder/decoder |
| [`tilemaker-output`](crates/tilemaker-output) | MBTiles, PMTiles v3 and directory writers |
| [`tilemaker-core`](crates/tilemaker-core) | the tile pipeline: reading through the profile, tile index, rendering |
| [`tilemaker-cli`](crates/tilemaker-cli) | the `tilemaker` binary |
| [`bindings/tilemaker-node`](crates/bindings/tilemaker-node) | the N-API binding, `@tilemaker-rs/node` |

## Verified against the originals

- **PBF reading**: element counts equal `osmium fileinfo -e` on extracts; the
  full planet (95 GB) decodes without errors.
- **Feature export** vs `osmium export` on the world's administrative
  boundaries and settlements (540 MB): the same 1,605,283 points with the
  same tags, and the same 249,381 boundary multipolygons with identical
  polygon, ring and point counts and areas for all but 3 (where a
  self-touching ring is split at a different node; same area).
- **Tiles** vs the C++ tilemaker with the OpenMapTiles profile on Monaco,
  Liechtenstein and a 2.6 GB Netherlands extract (48 million features):
  the same non-empty tiles, feature counts within 0.03% overall (within 1%
  per zoom and layer), total tile bytes within 0.5%. These comparisons
  run in the test suite when the extracts are present (see
  [fixtures](fixtures/README.md)).

## Performance

On a 10-core Apple M-series machine:

| task | this | reference |
|---|---|---|
| decode the planet (95 GB) | 156 s (610 MB/s), 0.6 GB RSS | |
| countries, regions, cities and towns straight from the planet (Node) | 152 s, 1.6 GB (disk-bound, cold cache) | `osmium tags-filter` + `osmium export`: 462 s + 129 s, 4.1 GB |
| world boundaries + cities from a 540 MB extract, as objects in Node | ~10 s, 2.6 GB | |
| the same, written as GeoJSON lines | 51 s, 2.8 GB | `osmium export`: 130 s, 4.1 GB |
| Netherlands+ (2.6 GB), OpenMapTiles profile → MBTiles | 96 s, 11.5 GB | C++ tilemaker: 99 s, 9.5 GB |
| Liechtenstein, OpenMapTiles profile → MBTiles | 1.3 s, 130 MB | C++ tilemaker: 2.0 s, 1.5 GB |

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — passes, stores, flow control
- [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) — what carries over from tilemaker
- [the Node package](crates/bindings/tilemaker-node/README.md)

## Building and testing

```sh
cargo test --workspace --exclude tilemaker-node   # Rust (≈140 tests)
cd crates/bindings/tilemaker-node
corepack yarn install && corepack yarn build && corepack yarn test   # Node (48 tests)
```

Rust 1.87+, Node 18+. LuaJIT and SQLite are built from source (vendored);
build with `--no-default-features --features lua54` for Lua 5.4 instead of
LuaJIT (simpler to cross-compile).

## Not supported (yet)

- shapefile/GeoJSON layers in the tile config (`source`), and the Lua
  spatial queries that read them (`Intersects`, `FindIntersecting`,
  `CoveredBy`, `FindCovering`, `AreaIntersecting` — they report nothing)
- decluttering (`declutter_below`, `Score()` is accepted and ignored)
- `--merge` into existing MBTiles, `--store` (on-disk stores for tile
  generation; the extractor has `nodeStoreFile`)
- lzma/lz4/zstd-compressed PBF blobs (rare; zlib and raw are supported)

## License

FTWPL, like tilemaker (see [LICENCE.txt](LICENCE.txt)).
