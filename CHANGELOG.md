# Changelog

## 0.1.0 — unreleased

First release.

### Node.js (`@tilemaker-rs/node`)

- `openPbf(path)` → `PbfReader` with `header`, `blockCount`, `size`.
- `reader.elements(options)` / `streamElements(path, options)`: nodes, ways
  (refs, optional coordinates) and relations (members), with osmium-style
  tag filters, tag selection, metadata, bbox.
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

### Rust / CLI

- `tilemaker` with the C++ tool's arguments (`--input/--output/--config/
  --process/--bbox/--threads`), MBTiles, PMTiles v3 and directory output.
- `tilemaker export` (GeoJSON lines) and `tilemaker inspect`.
- Crates: `tilemaker-pbf`, `-geom`, `-store`, `-export`, `-config`,
  `-lua`, `-mvt`, `-output`, `-core`, `-cli`.
