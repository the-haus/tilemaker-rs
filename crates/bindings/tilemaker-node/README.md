# @tilemaker-rs/node

Stream OpenStreetMap data out of `.osm.pbf` files in Node.js — fast, on all
cores, with bounded memory — and transform it however you like.

- **Elements**: nodes, ways (node ids, optionally coordinates) and relations
  (members), selected with osmium-style tag filters.
- **Features**: assembled geometries — points, linestrings, and
  multipolygons from closed ways and `multipolygon`/`boundary` relations —
  matching `osmium export` (verified on the whole planet's administrative
  boundaries: 249,381 of 249,381 relations).
- **Vector tiles**: tilemaker's pipeline (`process.lua` + `config.json`),
  streamed tile by tile or written to MBTiles, PMTiles or a directory.

The work happens in Rust (N-API); the JS thread only turns finished batches
into objects. Coordinates arrive as `Float64Array` views, so even a country
outline with millions of points never becomes a giant JS string.

```ts
import { openPbf, toGeoJSON } from "@tilemaker-rs/node";

const pbf = await openPbf("planet.osm.pbf");
for await (const f of pbf.features({ filters: ["n/place=city,town", "r/admin_level=2,4"] })) {
  f.type;     // "node" | "way" | "relation"
  f.id;       // 62508
  f.tags;     // { name: "Liechtenstein", admin_level: "2", … }
  f.geometry; // { type: "MultiPolygon", coordinates: Float64Array, parts, polygons }
  toGeoJSON(f); // a GeoJSON Feature, when you want one
}
```

## Install

```sh
npm install @tilemaker-rs/node
```

Prebuilt binaries: Linux (x64/arm64, glibc and musl), macOS (x64/arm64),
Windows (x64). Node.js 18 or newer.

## Why

| | `osmium export` (GeoJSON) | `@tilemaker-rs/node` (GeoJSON lines) | `@tilemaker-rs/node` (objects) |
|---|---|---|---|
| World boundaries + cities (540 MB extract) | 130 s, 4.1 GB | 51 s, 2.8 GB | **9.7 s**, 2.7 GB |

Decoding the full 95 GB planet runs at ~610 MB/s (156 s for 10.9 billion
nodes, 1.2 billion ways, 14.9 million relations) with a 574 MB footprint, on
a 10-core Apple M-series machine.

## Elements

```ts
const pbf = await openPbf("region.osm.pbf");

for await (const e of pbf.elements({ filters: ["w/highway", "r/type=route"], wayLocations: true })) {
  switch (e.type) {
    case "node":     e.lon; e.lat; break;
    case "way":      e.refs; /* Float64Array of node ids */ e.coordinates; /* [lon, lat, …] */ break;
    case "relation": e.members; /* [{ type, ref, role }] */ break;
  }
}
```

Without `wayLocations` this is a single pass in constant memory. With it, a
first pass finds the nodes the selected ways need and only those locations
are kept (compressed, ~10 bytes each).

### Straight from a download

`streamElements` also takes bytes as they arrive — a `fetch()` body, a Node
`Readable`, stdin, any (async) iterable of `Uint8Array`s — so a file never
has to touch the disk:

```ts
import { streamElements } from "@tilemaker-rs/node";

const res = await fetch("https://download.geofabrik.de/europe/monaco-latest.osm.pbf");
for await (const e of streamElements(res.body!, { filters: ["amenity"] })) {
  // decoded on worker threads as the bytes arrive
}
```

The source is read only as fast as you consume (at most ~8 MB ahead).
Reading bytes is a single pass, so it gives elements (not assembled
features), and `wayLocations` needs a file with `LocationsOnWays`.

## Features

```ts
const stream = pbf.features({
  filters: ["a/landuse=forest", "w/highway"],   // osmium tags-filter syntax
  geometryTypes: ["linestring", "polygon"],     // default: all
  closedWays: "auto",                           // "auto" | "polygon" | "linestring" | "both"
  tags: ["name", "highway", "landuse"],         // or true (default) / false
  bbox: [5.9, 50.7, 7.3, 53.6],                 // west, south, east, north
});
for await (const batch of stream.batches()) {
  // up to `batchSize` (4096) features at a time
}
console.log(await stream.stats); // counters: emitted, polygons, invalidAreas, storedNodes, …
```

### Geometries

Coordinates are flat `[lon, lat, lon, lat, …]` in a `Float64Array`:

| type | extra arrays |
|---|---|
| `Point` | — (`coordinates` is `[lon, lat]`) |
| `LineString` | — |
| `MultiLineString` | `parts`: exclusive end of each line, in points |
| `MultiPolygon` | `parts`: end of each ring; `polygons`: end of each polygon, in rings |

A polygon's first ring is its exterior (counter-clockwise), the rest are
holes (clockwise), as RFC 7946 asks. Closed ways and area relations are
always `MultiPolygon`s.

Helpers: `toGeoJSON(feature)`, `geometryToGeoJSON(geometry)`,
`polygonRings(geometry)` (ring ranges), `bbox(geometry)` and
`geoJSONSeq(stream)` (newline-delimited GeoJSON strings).

### How areas are assembled

Like osmium's area assembler: every member way of a `type=multipolygon` or
`type=boundary` relation counts (whatever its role, each way once);
segments shared by two rings cancel out; rings are split where they touch
themselves; and they nest by containment (inside an even number of rings
is an exterior). A relation with missing member ways (cut by an extract's
edge), unclosed rings or crossing segments yields nothing and is counted in
`stats.relationsMissingWays` / `stats.invalidAreas`, again like osmium.

### Filters

`[TYPES/]KEY`, `KEY=V1,V2`, `KEY!=V`, `!KEY`; `TYPES` is any of `n`, `w`,
`r` and `a` (areas: closed ways and multipolygon/boundary relations); keys
and values may use `*` wildcards (`name:*`, `*bahn*`). An element is kept
when any filter matches. With no filters every tagged element is kept.

## Vector tiles

```ts
import { generateTiles, writeTiles } from "@tilemaker-rs/node";

// Stream tiles (e.g. into your own storage or an HTTP response).
for await (const tile of generateTiles("monaco.osm.pbf", {
  config: "config.json",       // or the parsed object
  process: "process.lua",      // or { source: "function node_function() … end" }
  compression: "gzip",         // "none" (default) | "gzip" | "deflate" | "config"
})) {
  tile.z; tile.x; tile.y; tile.data; // Buffer
}

// Or write a tileset.
const stats = await writeTiles("monaco.osm.pbf", {
  config: "config.json",
  process: "process.lua",
  output: "monaco.pmtiles",    // .mbtiles, .pmtiles, or a directory
  onProgress: (p) => console.log(p),
});
stats.metadata.vector_layers;
```

Profiles and configs are tilemaker's (see its
[CONFIGURATION.md](https://github.com/systemed/tilemaker/blob/master/docs/CONFIGURATION.md));
the OpenMapTiles profile runs unchanged, and output matches the C++
tilemaker to within 1% of features per layer and zoom. Not supported:
shapefile/GeoJSON layers (`source` in the config, and the `Intersects`
family of Lua functions), decluttering, and merging into existing MBTiles.

## Streams

`features()`, `elements()` and `tiles()` return an `OsmStream`:

- `for await (const x of stream)` or `for await (const batch of stream.batches())`
- `stream.toArray()`, `stream.map(fn)`, `stream.filter(fn)`, `stream.toReadable()`
- `stream.stats` — a promise of the counters, resolved when it ends
- `stream.close()` — stop early (breaking out of a loop does this too)
- options `onProgress`, `signal` (an `AbortSignal`), `batchSize`, `readAhead`

Nothing is read until iteration starts. The producer never gets more than
`readAhead` batches (default 2) ahead of you: a slow consumer slows the
export down instead of filling memory. A stream can be iterated once.

```ts
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";
import { createWriteStream } from "node:fs";
import { geoJSONSeq, streamFeatures } from "@tilemaker-rs/node";

await pipeline(
  Readable.from(geoJSONSeq(streamFeatures("region.osm.pbf", { filters: ["amenity"] }))),
  createWriteStream("amenities.geojsonseq"),
);
```

## Memory

Only what the selection needs is kept:

1. selected relations mark their member ways;
2. selected and marked ways mark their nodes (compressed id sets, ~2 bytes
   per id);
3. marked nodes' locations are stored (~10 bytes each, delta-compressed);
4. marked ways' geometries are stored (varint-delta encoded);
5. relations are assembled from the stored ways.

Passes that can select nothing are skipped, and sorted files let each pass
decode only the blocks holding its element type. For selections that need
hundreds of millions of nodes, `nodeStoreFile: "/tmp/nodes.bin"` keeps node
locations in a sparse, memory-mapped file (8 bytes per possible id, only
touched pages use disk; the path must not exist and nothing is left
behind) instead of RAM.

## Errors

Invalid options, unreadable files and Lua errors reject with an
`OsmStreamError` — from `openPbf`, or from the first iteration of a stream.

## License

FTWPL, like tilemaker.
