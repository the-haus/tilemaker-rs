# @tilemaker-rs/node

Stream OpenStreetMap data out of `.osm.pbf` files in Node.js — fast, on all
cores, with bounded memory — and transform it however you like.

- **Elements**: nodes, ways (node ids, optionally coordinates) and relations
  (members), selected with osmium-style tag filters.
- **Features**: assembled geometries — points, linestrings, and
  multipolygons from closed ways and `multipolygon`/`boundary` relations —
  matching `osmium export` (verified on the whole planet's administrative
  boundaries: 249,381 of 249,381 relations), optionally clipped,
  simplified and rounded, and enriched with area, length, centroid, label
  point, bounding box, parent relations and the areas they lie in.
- **GeoJSON files** written natively (`writeGeoJSONSeq`), without a single
  feature crossing into JavaScript.
- **Extracts** as PBF, identical to `osmium tags-filter` and `osmium
  extract` output.
- **Reverse geocoding**: a polygon index for point-in-polygon lookups
  (which country and region is this place in?).
- **File statistics** like `osmium fileinfo -e`.
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

<!-- bench:node -->

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
are kept (compressed, ~6 bytes each).

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

### Clipping, simplifying, rounding

Applied natively, in this order, before a feature is emitted:

```ts
pbf.features({
  filters: ["a/natural=water"],
  clip: [4.7, 52.3, 5.0, 52.45],  // cut to a box (unlike bbox, which selects)
  simplify: 0.0001,               // Douglas–Peucker tolerance in degrees
  precision: 6,                   // decimals
});
```

Clipping can turn a line into a `MultiLineString`; rings that collapse
(under clipping, simplification or rounding) are dropped, and with an
exterior its polygon.

### Computed values

`enrich` adds values computed natively from the emitted geometry:

```ts
for await (const f of pbf.features({
  filters: ["a/leisure=park", "w/highway=cycleway"],
  enrich: {
    area: true,        // f.area: m² on the sphere (0 for points and lines)
    length: true,      // f.length: m (lines; polygons' perimeter)
    centroid: true,    // f.centroid: [lon, lat], area/length weighted
    labelPoint: true,  // f.labelPoint: inside polygons (pole of inaccessibility), lines' midpoint
    bbox: true,        // f.bbox: [west, south, east, north]
    memberOf: ["type=route"],        // f.memberOf: [{ id, role, tags }] parent relations (or true: all)
    within: ["r/admin_level=2,4"],   // f.within: [{ type, id, tags }] areas it lies in, smallest first
  },
})) {
  // …
}
```

`memberOf` costs a pass over the relations (keeping their memberships);
`within` reads the selected areas first and looks up each feature's point
(its midpoint or label point for lines and polygons) — a reverse-geocoding
join in one call. Both are cheap with narrow filters.

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
Filters are resolved against each block's string table once, then matched
by comparing integers.

## GeoJSON files

`writeGeoJSONSeq` exports straight to a newline-delimited GeoJSON file:
filtering, assembly, serialisation and writing all happen on native
threads, and each line is what `toGeoJSON` makes of the feature.

```ts
import { writeGeoJSONSeq } from "@tilemaker-rs/node";

const stats = await writeGeoJSONSeq("planet.osm.pbf", {
  filters: ["r/admin_level=2"],
  enrich: { area: true, labelPoint: true },  // as @area, @labelPoint
  output: "countries.geojsonseq",
  recordSeparator: false,                    // true: RFC 8142 (osmium's default)
});
```

Coordinates are written from their 1e-7 degree fixed point (exact, no
float formatting); properties are the tags plus `@type`, `@id`, metadata
(`@version`, `@timestamp`, …) and computed values (`@area`, `@within`, …);
a computed `bbox` is GeoJSON's `bbox` member.

## Extracts

`extract` writes a PBF file as `osmium extract` (with its default
`complete_ways` strategy) and `osmium tags-filter` would — in one run, on
all cores, with tags and metadata kept. The output is identical to
osmium's.

```ts
import { extract } from "@tilemaker-rs/node";

// osmium tags-filter planet.osm.pbf n/place=city,town r/admin_level=2,4
await extract("planet.osm.pbf", {
  output: "geography.osm.pbf",
  filters: ["n/place=city,town", "r/admin_level=2,4"],
});

// osmium extract -b … then osmium tags-filter … w/highway
await extract("netherlands.osm.pbf", {
  output: "amsterdam-roads.osm.pbf",
  bbox: [4.7, 52.3, 5.0, 52.45],    // or polygon: GeoJSON object, .geojson or .poly path
  filters: ["w/highway"],
  omitReferenced: false,            // true: osmium tags-filter -R
});
```

- A **region** keeps the nodes inside, every way with a node inside (with
  all its nodes), every relation with such a node or way as a member, and
  the relations above those.
- **Filters** keep matching elements, the nodes of matching ways, and the
  members of matching relations, recursively through member relations.

## Reverse geocoding

`PolygonIndex` answers "which of these areas contain this point?",
smallest first:

```ts
import { openPbf, PolygonIndex } from "@tilemaker-rs/node";

const areas = await PolygonIndex.fromFile("planet.osm.pbf", {
  filters: ["r/admin_level=2,4"],
  tags: ["name", "admin_level", "ISO3166-1"],
});
areas.lookup(4.9, 52.37).map((a) => a.tags.name);         // ["Noord-Holland", "Nederland"]
areas.find(4.9, 52.37, (a) => a.tags.admin_level === "2"); // the smallest matching area
const at = await areas.findMany(lonLats);                  // Int32Array of entry indices (-1: none), on worker threads
```

Polygon parts sit in an R-tree; each ring keeps its points at 1e-7 degrees
with a banded index of its edges, so a lookup in a million-point coastline
tests about a thousand edges, exactly (integer arithmetic). A point on a
boundary counts as inside; holes are respected. `fromFeatures` indexes any
iterable of features. For a one-off join, `enrich.within` does the same
inside a feature stream.

## File statistics

```ts
import { fileInfo } from "@tilemaker-rs/node";

const info = await fileInfo("planet.osm.pbf", { extended: true });
info.header;        // bbox, features, writing program, replication
info.compression;   // { zlib: 53186 }
info.data.counts;   // { nodes, ways, relations }
info.data.minId; info.data.maxId; info.data.bbox; info.data.timestamps;
info.data.objectsOrdered; info.data.metadata; // { all: [...], some: [...] }
```

Without `extended` only the header and blob index are read (instant);
with it every element is decoded, on all cores (`osmium fileinfo -e`).

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

(`writeGeoJSONSeq` does the same several times faster.)

## Memory

Only what the selection needs is kept:

1. selected relations mark their member ways;
2. selected and marked ways mark their nodes (compressed id sets, ~2 bytes
   per id);
3. marked nodes' locations are stored (~6 bytes each, delta-compressed);
4. marked ways' geometries are stored (varint-delta encoded);
5. relations are assembled from the stored ways.

Passes that can select nothing are skipped; sorted files let each pass
decode only the blocks holding its element type, and passes that need only
marked ids skip blocks whose id range holds none (found by inflating just
the start of each block). For selections that need hundreds of millions of
nodes, `nodeStoreFile: "/tmp/nodes.bin"` keeps node locations in a sparse,
memory-mapped file (8 bytes per possible id, only touched pages use disk;
the path must not exist and nothing is left behind) instead of RAM.

## API reference

| export | |
|---|---|
| `openPbf(path)` → `Promise<PbfReader>` | open and index a file |
| `PbfReader` | `.header` (`PbfHeader`), `.blockCount`, `.size`, `.path`, `.elements(opts)`, `.features(opts)`, `.writeGeoJSONSeq(opts)`, `.extract(opts)`, `.fileInfo(opts)`, `.tiles(opts)`, `.writeTiles(opts)` |
| `streamElements(path \| ByteSource, ElementOptions)` | elements from a path or from bytes |
| `streamFeatures(path, FeatureOptions)` | features from a path |
| `writeGeoJSONSeq(path, WriteGeoJSONSeqOptions)` → `Promise<ExportStats>` | features into a GeoJSON lines file, natively |
| `extract(path, ExtractOptions)` → `Promise<ExtractStats>` | a PBF extract (osmium `tags-filter` / `extract`) |
| `fileInfo(path, FileInfoOptions)` → `Promise<FileInfo>` | what a file holds (osmium `fileinfo`) |
| `PolygonIndex` | `.fromFile(path, opts)`, `.fromFeatures(iterable)`, `.entries`, `.size`, `.lookup(lon, lat)`, `.find(lon, lat, predicate?)`, `.findMany(points)` |
| `generateTiles(path, TileStreamOptions)` | tiles from a path |
| `writeTiles(path, WriteTilesOptions)` → `Promise<TileStats>` | write a tileset |
| `OsmStream<T>` | see [Streams](#streams) |
| `toGeoJSON`, `geometryToGeoJSON`, `geoJSONSeq`, `polygonRings`, `bbox` | geometry helpers |
| `version()` | the native library's version |
| `OsmStreamError` | the error class |

Types: `Element` = `NodeElement | WayElement | RelationElement` (with
`Member`, `Meta`, `Tags`, `OsmType`); `Feature` with `Geometry` =
`PointGeometry | LineStringGeometry | MultiLineStringGeometry |
MultiPolygonGeometry` and the computed `ParentRelation`, `AreaRef`;
`Tile`; options `CommonOptions`, `ElementOptions`, `FeatureOptions`,
`EnrichOptions`, `WriteGeoJSONSeqOptions`, `ExtractOptions`,
`FileInfoOptions`, `TileOptions`, `TileStreamOptions`,
`WriteTilesOptions`; results `ExportStats`, `ExtractStats`, `FileInfo`
(`FileData`, `PerType`), `TileStats`, `PolygonEntry`; progress `Progress`
(`Phase`) and `TileProgress`; `GeoJSONFeature`, `GeoJSONGeometry`,
`Position`; `ByteSource`. Every field is documented in the type
declarations.

## Examples

In [`examples/`](examples): `geography.mjs` (countries, regions, cities
and towns from a planet), `places-in-regions.mjs` (each place's country
and region with a `PolygonIndex`), `extract.mjs`, `parks-csv.mjs` (areas
and label points), `pois-csv.mjs`, `export-geojsonseq.mjs`, `tiles.mjs`.

## Errors

Invalid options, unreadable files and Lua errors reject with an
`OsmStreamError` — from `openPbf`, or from the first iteration of a stream.

## License

FTWPL, like tilemaker.
