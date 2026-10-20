# Changelog

## 0.1.0 — 2026-10-20

First release.

### Faster, smaller

- PBF decoding: libdeflate for zlib blobs, a single-byte varint fast path,
  the string table in one buffer and reused decode buffers — the
  Netherlands (2.6 GB) decodes in 2.4 s (osmium fileinfo -e: 4.8 s).
- Tag filters are resolved to string-table indices once per block.
- Passes that need only marked nodes or ways (relation-only selections)
  skip blocks whose id range holds none, by inflating only the start of
  each block: Netherlands `r/admin_level=2,4` 3.3 s → 1.8 s.
- Node locations are delta-coded (~6 bytes a node instead of ~9).
- Tiles: a packed geometry store (no allocation per geometry), 16-byte
  output objects, interned attribute sets, the node store released before
  relations, allocation-free `Find`/`Holds` and reused Lua objects, merging
  in file order — Netherlands with the OpenMapTiles profile 93 s / 12.5 GB
  → 63 s / 5.7 GB (C++ tilemaker: 91 s / 9.4 GB).
- GeoJSON is written natively, serialised on the workers: the CLI's
  `export` is ~3× faster.

### New

- Feature options: `clip`, `simplify`, `precision`; `enrich` with `area`,
  `length`, `centroid`, `labelPoint`, `bbox`, `memberOf` (parent
  relations) and `within` (the areas a feature lies in).
- `writeGeoJSONSeq()` (Node) and `tilemaker export` write GeoJSON lines
  without features crossing into JS.
- `extract()` / `tilemaker extract`: PBF extracts identical to `osmium
  tags-filter` and `osmium extract` (bbox, GeoJSON or `.poly` polygons).
- `PolygonIndex` (Node) / `tilemaker_export::PolygonIndex`: point-in-polygon
  lookups and reverse geocoding.
- `fileInfo()` / `tilemaker fileinfo [-e]`: file statistics like `osmium
  fileinfo -e`.
- `tilemaker_pbf::ParallelWriter` and `BlockBuilder`: parallel PBF writing.
- Benchmarks against osmium, C++ tilemaker, osm-pbf-parser and osm-read in
  [`bench/`](bench).

### Node.js (`@tilemaker-rs/node`)

- `openPbf(path)` → `PbfReader` with `header`, `blockCount`, `size`.
- `reader.elements(options)` / `streamElements(path, options)`: nodes, ways
  (refs, optional coordinates) and relations (members), with osmium-style
  tag filters, tag selection, metadata, bbox.
- `streamElements(bytes, options)`: elements from a `fetch()` body, a
  `Readable`, stdin or any (async) iterable of bytes, with backpressure.
- `reader.features(options)` / `streamFeatures(path, options)`: points,
  linestrings, multi-linestrings and multipolygons with flat `Float64Array`
  coordinates; `closedWays`, `geometryTypes`, `relationLines`,
  `nodeStoreFile` for planet-scale selections.
- `reader.tiles(options)` / `generateTiles(path, options)`: vector tiles
  from a tilemaker profile and config, streamed; `writeTiles` writes
  MBTiles, PMTiles or a directory.
- `OsmStream`: async iteration by record or batch, `toArray`, `map`,
  `filter`, `toReadable`, `stats`, `close`, `onProgress`, `signal`,
  credit-based backpressure (`readAhead`).
- GeoJSON helpers: `toGeoJSON`, `geometryToGeoJSON`, `geoJSONSeq`,
  `polygonRings`, `bbox`.

### Verified

- Feature export matches `osmium export` on the planet's administrative
  boundaries (249,381 relations) and settlements (1,605,283 points).
- Tiles match the C++ tilemaker with the OpenMapTiles profile to within
  0.03% of features (Monaco, Liechtenstein, a 2.6 GB Netherlands extract).

### Rust / CLI

- `tilemaker` with the C++ tool's arguments (`--input/--output/--config/
  --process/--bbox/--threads`), MBTiles, PMTiles v3 and directory output.
- `tilemaker export` (GeoJSON lines), `tilemaker extract` (PBF),
  `tilemaker fileinfo` and `tilemaker inspect`.
- Crates: `tilemaker-pbf`, `-geom`, `-store`, `-export`, `-config`,
  `-lua`, `-mvt`, `-output`, `-core`, `-cli`.
