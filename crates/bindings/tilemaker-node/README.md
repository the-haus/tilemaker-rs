# @tilemaker-rs/node

[![npm version](https://img.shields.io/npm/v/@tilemaker-rs/node.svg)](https://www.npmjs.com/package/@tilemaker-rs/node)
[![Node.js](https://img.shields.io/node/v/@tilemaker-rs/node.svg)](#requirements)
[![platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macOS%20%7C%20windows-blue.svg)](#supported-platforms)
[![types](https://img.shields.io/npm/types/@tilemaker-rs/node.svg)](#typescript)
[![license](https://img.shields.io/npm/l/@tilemaker-rs/node.svg)](#license)

**Fast, streaming OpenStreetMap processing for Node.js.** Read `.osm.pbf`
files on every core with bounded memory, and get filtered elements,
assembled geometries, GeoJSON, PBF extracts, reverse geocoding, file
statistics and vector tiles. The engine is written in Rust and exposed
through N-API, and prebuilt binaries are provided for eight platforms.

```ts
import { openPbf, toGeoJSON } from "@tilemaker-rs/node";

const pbf = await openPbf("planet.osm.pbf");
for await (const f of pbf.features({ filters: ["n/place=city,town", "r/admin_level=2,4"] })) {
  f.type;       // "node" | "way" | "relation"
  f.id;         // 62508
  f.tags;       // { name: "Liechtenstein", admin_level: "2", … }
  f.geometry;   // { type: "MultiPolygon", coordinates: Float64Array, parts, polygons }
  toGeoJSON(f); // a GeoJSON Feature, when you want one
}
```

## Contents

- [Highlights](#highlights)
- [Installation](#installation)
  - [Requirements](#requirements)
  - [Supported platforms](#supported-platforms)
- [Versioning and releases](#versioning-and-releases)
- [Performance](#performance)
- [Guide](#guide)
  - [Elements](#elements) · [Features](#features) · [GeoJSON files](#geojson-files) · [Extracts](#extracts)
  - [Reverse geocoding](#reverse-geocoding) · [File statistics](#file-statistics) · [Vector tiles](#vector-tiles)
  - [Streams](#streams) · [Memory](#memory)
- [API reference](#api-reference)
- [TypeScript](#typescript)
- [Errors](#errors)
- [Troubleshooting](#troubleshooting)
- [Building from source](#building-from-source)
- [Examples](#examples)
- [Changelog](#changelog) · [License](#license)

## Highlights

- **Elements**: nodes, ways (node ids, optionally coordinates) and relations
  (members), selected with osmium-style tag filters.
- **Features**: assembled geometries, matching `osmium export`. These are
  points, linestrings, and multipolygons built from closed ways and from
  `multipolygon`/`boundary` relations. On the whole planet's administrative
  boundaries the output matches for 249,381 of 249,381 relations. Features
  can be clipped, simplified and rounded, and enriched with area, length,
  centroid, label point, bounding box, parent relations and the areas they
  lie in.
- **GeoJSON files**: written natively (`writeGeoJSONSeq`), without a single
  feature crossing into JavaScript.
- **Extracts**: PBF output identical to `osmium tags-filter` and
  `osmium extract`.
- **Reverse geocoding**: a polygon index for point-in-polygon lookups, such
  as which country and region a place is in.
- **File statistics**: like `osmium fileinfo -e`.
- **Vector tiles**: tilemaker's pipeline (`process.lua` + `config.json`),
  streamed tile by tile or written to MBTiles, PMTiles or a directory.

The work happens in Rust on native threads. The JavaScript thread only
turns finished batches into objects. Coordinates arrive as `Float64Array`
views, so even a country outline with millions of points never becomes a
large JavaScript array or string. Backpressure is built in: a slow consumer
slows the reader down instead of filling memory.

## Installation

```sh
npm install @tilemaker-rs/node
# or
yarn add @tilemaker-rs/node
# or
pnpm add @tilemaker-rs/node
```

The package ships no install scripts and needs no compiler. The prebuilt
binary for your platform is a separate package,
`@tilemaker-rs/node-<platform>`, which the package manager installs
automatically through `optionalDependencies`. Only the one matching your
system is downloaded.

### Requirements

| | Supported | Notes |
|---|---|---|
| Node.js | **18 or newer** | N-API version 6. Tested on Node 20, 22 and 24 in CI; every release is also installed and run on Node 18 on every platform before it is published. |
| Module systems | ESM and CommonJS | `import` and `require()` both work. |
| TypeScript | optional | Declarations are bundled; no `@types` package is needed. |

### Supported platforms

| OS | Architecture | C library | Binary package | Minimum OS |
|---|---|---|---|---|
| Linux | x64 | glibc | `@tilemaker-rs/node-linux-x64-gnu` | glibc 2.31 (Debian 11, Ubuntu 20.04, RHEL 9, Amazon Linux 2023) |
| Linux | arm64 | glibc | `@tilemaker-rs/node-linux-arm64-gnu` | glibc 2.31 |
| Linux | x64 | musl | `@tilemaker-rs/node-linux-x64-musl` | Alpine 3.x |
| Linux | arm64 | musl | `@tilemaker-rs/node-linux-arm64-musl` | Alpine 3.x |
| macOS | x64 (Intel) | — | `@tilemaker-rs/node-darwin-x64` | macOS 10.15 |
| macOS | arm64 (Apple Silicon) | — | `@tilemaker-rs/node-darwin-arm64` | macOS 11 |
| Windows | x64 | MSVC | `@tilemaker-rs/node-win32-x64-msvc` | Windows 10 |
| Windows | arm64 | MSVC | `@tilemaker-rs/node-win32-arm64-msvc` | Windows 10 |

Every binary is built natively on its own OS and architecture with LuaJIT
for tile profiles, and the full test suite runs against it before release.
For any other platform, see [Building from source](#building-from-source).

## Versioning and releases

- **Semantic versioning.** While the package is at `0.x`, a minor release
  (`0.1` → `0.2`) may contain breaking changes and a patch release will
  not. From `1.0.0` on, breaking changes happen only in major releases.
- **Dist-tags.** Stable releases are published under `latest`, and
  prereleases (such as `0.2.0-rc.1`) under `next`:
  `npm install @tilemaker-rs/node@next`.
- **One version for everything.** The root package and the platform
  packages always share a version, and the root package pins its platform
  packages exactly. The version also matches the `tilemaker` CLI and the
  Rust crates built from the same release tag.
- **Provenance.** Releases are built and published from CI with
  [npm provenance](https://docs.npmjs.com/generating-provenance-statements),
  so every tarball can be traced to the commit and workflow that produced
  it.
- **Release notes** are in the [changelog](https://github.com/the-haus/tilemaker-rs/blob/main/CHANGELOG.md).

At runtime, `version()` returns the version of the native library that was
loaded.

### Bundled components

The native library is self-contained and links nothing from the system
beyond the C runtime. The 0.1 release line bundles:

| Component | Version | Used for |
|---|---|---|
| LuaJIT | 2.1 (rolling release, commit `c6ffc14`) | `process.lua` tile profiles (Lua 5.1 semantics, as in tilemaker) |
| SQLite | 3.53.2 | MBTiles output |
| zstd | 1.5.7 | zstd-compressed PBF blobs |
| libdeflate | 1.26 | zlib-compressed PBF blobs |

### Formats

| Format | Read | Write |
|---|---|---|
| OSM PBF (zlib, zstd, lz4 and uncompressed blobs) | ✓ | ✓ (`extract`) |
| GeoJSON / GeoJSON Text Sequences (RFC 7946, RFC 8142) | — | ✓ |
| Mapbox Vector Tiles | — | ✓ |
| MBTiles | — | ✓ |
| PMTiles v3 | — | ✓ |

OSM XML, OPL, history and change files, and lzma or bzip2-compressed PBF
blobs are not supported.

## Performance

Measured on an Apple M1 Max (10 cores, 32 GiB, macOS 26) with Node 24
against osmium 1.19.1 and the JavaScript PBF readers
[osm-pbf-parser](https://github.com/substack/osm-pbf-parser) and
[osm-read](https://github.com/marook/osm-read). Times are wall clock;
memory is peak resident.

**Reading into JavaScript objects** (the Netherlands and surroundings,
2.6 GB, 226 M nodes):

| Task | @tilemaker-rs/node | osm-pbf-parser | osm-read |
|---|---|---|---|
| Every element | **22.4 s** · 738 MB | 89.0 s · 451 MB | 160.8 s · 309 MB |
| Nodes with an `amenity` tag | **1.9 s** · 240 MB | 87.8 s · 435 MB | 156.5 s · 305 MB |

**Assembled features as GeoJSON** (cities, towns and admin level 2 and 4
boundaries):

| Input | `writeGeoJSONSeq` | `features()` (as objects) | osmium tags-filter + export |
|---|---|---|---|
| Netherlands (2.6 GB) | **2.2 s** · 189 MB | 2.2 s · 187 MB | 14.8 s · 2.2 GB |
| Planet (95 GB) | **1 min 59 s** · 958 MB | 1 min 59 s · 1.3 GB | 10 min 27 s · 4.2 GB |

The benchmark harness and results for the CLI, extracts and tiles are in
the repository's [`bench/`](https://github.com/the-haus/tilemaker-rs/tree/main/bench) directory.

## Guide

### Elements

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
first pass finds the nodes the selected ways need, and only those locations
are kept (compressed, about 6 bytes each).

#### Straight from a download

`streamElements` also takes bytes as they arrive: a `fetch()` body, a Node
`Readable`, stdin, or any (async) iterable of `Uint8Array`s. A file never
has to touch the disk:

```ts
import { streamElements } from "@tilemaker-rs/node";

const res = await fetch("https://download.geofabrik.de/europe/monaco-latest.osm.pbf");
for await (const e of streamElements(res.body!, { filters: ["amenity"] })) {
  // decoded on worker threads as the bytes arrive
}
```

The source is read only as fast as you consume, at most about 8 MB ahead.
Reading bytes is a single pass, so it yields elements rather than assembled
features, and `wayLocations` needs a file with `LocationsOnWays`.

#### Filters

Filters use osmium `tags-filter` syntax: `[TYPES/]KEY`, `KEY=V1,V2`,
`KEY!=V` and `!KEY`. `TYPES` is any of `n`, `w`, `r` and `a` (areas: closed
ways and multipolygon/boundary relations). Keys and values may use `*`
wildcards (`name:*`, `*bahn*`). An element is kept when any filter matches;
with no filters, every tagged element is kept. Filters are resolved against
each block's string table once and then matched by comparing integers.

### Features

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

#### Geometries

Coordinates are flat `[lon, lat, lon, lat, …]` in a `Float64Array`:

| Type | Extra arrays |
|---|---|
| `Point` | none (`coordinates` is `[lon, lat]`) |
| `LineString` | none |
| `MultiLineString` | `parts`: the exclusive end of each line, in points |
| `MultiPolygon` | `parts`: the end of each ring; `polygons`: the end of each polygon, in rings |

A polygon's first ring is its exterior (counter-clockwise) and the rest are
holes (clockwise), as RFC 7946 requires. Closed ways and area relations are
always `MultiPolygon`s.

Helpers: `toGeoJSON(feature)`, `geometryToGeoJSON(geometry)`,
`polygonRings(geometry)` (ring ranges), `bbox(geometry)` and
`geoJSONSeq(stream)` (newline-delimited GeoJSON strings).

#### Clipping, simplifying, rounding

Applied natively, in this order, before a feature is emitted:

```ts
pbf.features({
  filters: ["a/natural=water"],
  clip: [4.7, 52.3, 5.0, 52.45],  // cut to a box (unlike bbox, which selects)
  simplify: 0.0001,               // Douglas–Peucker tolerance in degrees
  precision: 6,                   // decimals
});
```

Clipping can turn a line into a `MultiLineString`. Rings that collapse
under clipping, simplification or rounding are dropped; when an exterior
ring is dropped, its whole polygon goes with it.

#### Computed values

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

`memberOf` costs a pass over the relations to keep their memberships.
`within` reads the selected areas first and then looks up each feature's
point (for lines and polygons, the midpoint or label point), which makes it
a reverse-geocoding join in one call. Both are cheap with narrow filters.

#### How areas are assembled

Areas are assembled like osmium's area assembler:

- Every member way of a `type=multipolygon` or `type=boundary` relation
  counts, whatever its role, and each way counts once.
- Segments shared by two rings cancel out.
- Rings are split where they touch themselves.
- Rings nest by containment: a ring inside an even number of other rings
  is an exterior.

A relation with missing member ways (cut by an extract's edge), unclosed
rings or crossing segments yields nothing, as in osmium. It is counted in
`stats.relationsMissingWays` or `stats.invalidAreas`.

### GeoJSON files

`writeGeoJSONSeq` exports straight to a newline-delimited GeoJSON file.
Filtering, assembly, serialisation and writing all happen on native
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

- **Coordinates** are written from their 1e-7 degree fixed point, so they
  are exact and need no float formatting.
- **Properties** are the tags, plus `@type`, `@id`, the metadata
  (`@version`, `@timestamp`, …) and the computed values (`@area`,
  `@within`, …).
- A computed `bbox` becomes GeoJSON's `bbox` member.

### Extracts

`extract` writes a PBF file the way `osmium extract` (with its default
`complete_ways` strategy) and `osmium tags-filter` would, in one run, on
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

- A **region** keeps the nodes inside it, every way with a node inside
  (with all of that way's nodes), every relation with such a node or way as
  a member, and the relations above those.
- **Filters** keep matching elements, the nodes of matching ways, and the
  members of matching relations, recursively through member relations.

### Reverse geocoding

`PolygonIndex` answers "which of these areas contain this point?",
smallest first:

```ts
import { PolygonIndex } from "@tilemaker-rs/node";

const areas = await PolygonIndex.fromFile("planet.osm.pbf", {
  filters: ["r/admin_level=2,4"],
  tags: ["name", "admin_level", "ISO3166-1"],
});
areas.lookup(4.9, 52.37).map((a) => a.tags.name);         // ["Noord-Holland", "Nederland"]
areas.find(4.9, 52.37, (a) => a.tags.admin_level === "2"); // the smallest matching area
const at = await areas.findMany(lonLats);                  // Int32Array of entry indices (-1: none), on worker threads
```

Polygon parts sit in an R-tree. Each ring keeps its points at 1e-7 degrees
with a banded index of its edges, so a lookup in a coastline of a million
points tests about a thousand edges, exactly, in integer arithmetic. A
point on a boundary counts as inside, and holes are respected.
`fromFeatures` indexes any iterable of features. For a one-off join,
`enrich.within` does the same inside a feature stream.

### File statistics

```ts
import { fileInfo } from "@tilemaker-rs/node";

const info = await fileInfo("planet.osm.pbf", { extended: true });
info.header;        // bbox, features, writing program, replication
info.compression;   // { zlib: 53186 }
info.data.counts;   // { nodes, ways, relations }
info.data.minId; info.data.maxId; info.data.bbox; info.data.timestamps;
info.data.objectsOrdered; info.data.metadata; // { all: [...], some: [...] }
```

Without `extended`, only the header and the blob index are read, which is
instant. With it, every element is decoded on all cores, like
`osmium fileinfo -e`.

### Vector tiles

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

Profiles and configs are tilemaker's; see its
[CONFIGURATION.md](https://github.com/systemed/tilemaker/blob/master/docs/CONFIGURATION.md).
The OpenMapTiles profile runs unchanged, and the output matches the C++
tilemaker to within 1% of features per layer and zoom.

Not supported:

- shapefile and GeoJSON layers (`source` in the config) and the `Intersects`
  family of Lua functions;
- decluttering;
- merging into existing MBTiles.

### Streams

`features()`, `elements()` and `tiles()` return an `OsmStream`:

- `for await (const x of stream)` or `for await (const batch of stream.batches())`
- `stream.toArray()`, `stream.map(fn)`, `stream.filter(fn)`, `stream.toReadable()`
- `stream.stats`: a promise of the counters, resolved when the stream ends
- `stream.close()`: stop early (breaking out of a loop does this too)
- options `onProgress`, `signal` (an `AbortSignal`), `batchSize` and `readAhead`

Nothing is read until iteration starts. The producer never gets more than
`readAhead` batches (default 2) ahead of you, so a slow consumer slows the
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

`writeGeoJSONSeq` does the same several times faster.

### Memory

Only what the selection needs is kept:

1. Selected relations mark their member ways.
2. Selected and marked ways mark their nodes (compressed id sets, about
   2 bytes per id).
3. The locations of marked nodes are stored (about 6 bytes each,
   delta-compressed).
4. The geometries of marked ways are stored (varint-delta encoded).
5. Relations are assembled from the stored ways.

Passes that can select nothing are skipped. In sorted files, each pass
decodes only the blocks holding its element type, and passes that need
only marked ids skip blocks whose id range holds none (found by inflating
just the start of each block).

For selections that need hundreds of millions of nodes,
`nodeStoreFile: "/tmp/nodes.bin"` keeps node locations in a sparse,
memory-mapped file instead of RAM. It takes 8 bytes per possible id, only
touched pages use disk, the path must not exist beforehand, and nothing is
left behind.

## API reference

| Export | Description |
|---|---|
| `openPbf(path)` → `Promise<PbfReader>` | Open and index a file. |
| `PbfReader` | `.header` (`PbfHeader`), `.blockCount`, `.size`, `.path`, `.elements(opts)`, `.features(opts)`, `.writeGeoJSONSeq(opts)`, `.extract(opts)`, `.fileInfo(opts)`, `.tiles(opts)`, `.writeTiles(opts)` |
| `streamElements(path \| ByteSource, ElementOptions)` | Elements from a path or from bytes. |
| `streamFeatures(path, FeatureOptions)` | Features from a path. |
| `writeGeoJSONSeq(path, WriteGeoJSONSeqOptions)` → `Promise<ExportStats>` | Features into a GeoJSON lines file, natively. |
| `extract(path, ExtractOptions)` → `Promise<ExtractStats>` | A PBF extract (osmium `tags-filter` / `extract`). |
| `fileInfo(path, FileInfoOptions)` → `Promise<FileInfo>` | What a file holds (osmium `fileinfo`). |
| `PolygonIndex` | `.fromFile(path, opts)`, `.fromFeatures(iterable)`, `.entries`, `.size`, `.lookup(lon, lat)`, `.find(lon, lat, predicate?)`, `.findMany(points)` |
| `generateTiles(path, TileStreamOptions)` | Tiles from a path. |
| `writeTiles(path, WriteTilesOptions)` → `Promise<TileStats>` | Write a tileset. |
| `OsmStream<T>` | See [Streams](#streams). |
| `toGeoJSON`, `geometryToGeoJSON`, `geoJSONSeq`, `polygonRings`, `bbox` | Geometry helpers. |
| `version()` | The version of the loaded native library. |
| `OsmStreamError` | The error class. |

## TypeScript

Type declarations are bundled, and every option and field is documented in
them. The main types are:

- **Elements**: `Element` = `NodeElement | WayElement | RelationElement`,
  with `Member`, `Meta`, `Tags` and `OsmType`.
- **Features**: `Feature`, with `Geometry` = `PointGeometry |
  LineStringGeometry | MultiLineStringGeometry | MultiPolygonGeometry`, and
  the computed `ParentRelation` and `AreaRef`.
- **Tiles**: `Tile`.
- **Options**: `CommonOptions`, `ElementOptions`, `FeatureOptions`,
  `EnrichOptions`, `WriteGeoJSONSeqOptions`, `ExtractOptions`,
  `FileInfoOptions`, `TileOptions`, `TileStreamOptions` and
  `WriteTilesOptions`.
- **Results**: `ExportStats`, `ExtractStats`, `FileInfo` (with `FileData`
  and `PerType`), `TileStats` and `PolygonEntry`.
- **Progress**: `Progress` (with `Phase`) and `TileProgress`.
- **GeoJSON**: `GeoJSONFeature`, `GeoJSONGeometry` and `Position`.
- **Input**: `ByteSource`.

## Errors

Invalid options, unreadable files and Lua errors reject with an
`OsmStreamError`, either from `openPbf` or from the first iteration of a
stream.

```ts
import { openPbf, OsmStreamError } from "@tilemaker-rs/node";

try {
  for await (const f of (await openPbf("region.osm.pbf")).features({ filters: ["w/highway"] })) {
    // …
  }
} catch (err) {
  if (err instanceof OsmStreamError) console.error("could not read the file:", err.message);
  else throw err;
}
```

## Troubleshooting

### No native binary found

`@tilemaker-rs/node: no native binary found for <platform>-<arch>` means
the platform package was not installed. Common causes:

- **Optional dependencies were skipped.** For example, `npm install
  --omit=optional`, `--no-optional`, or `optional=false` in `.npmrc`.
  Install without that option.
- **`node_modules` came from another platform.** For example, it was
  installed on macOS and copied into a Linux Docker image, or installed on
  glibc and run on Alpine. Install inside the image or on the target
  system instead. To install binaries for several platforms on purpose,
  use Yarn's [`supportedArchitectures`](https://yarnpkg.com/configuration/yarnrc#supportedArchitectures)
  or pnpm's [`supportedArchitectures`](https://pnpm.io/settings#supportedarchitectures).
- **The platform is not supported.** See [Supported platforms](#supported-platforms)
  and [Building from source](#building-from-source).

### `GLIBC_2.xx` not found

An error like `version 'GLIBC_2.xx' not found` means the Linux
distribution is older than glibc 2.31. Upgrade the base image
(for example to Debian 11 or Ubuntu 20.04 or later), or use an Alpine
image, which gets the musl build.

### Bundlers (webpack, esbuild, Vite, Next.js)

The package loads a native `.node` addon, so it must stay external to the
bundle. Mark it as an external, or use `serverExternalPackages` in
Next.js.

## Building from source

You need Rust 1.87 or newer, Node.js 18 or newer, and a C compiler (LuaJIT,
SQLite, zstd and libdeflate are compiled from vendored sources). From the
root of the repository:

```sh
corepack enable
yarn install
cd crates/bindings/tilemaker-node
yarn build        # native addon (release) + TypeScript
yarn test         # vitest
```

`yarn build` writes `tilemaker-node.<platform>.node` next to the package,
and the loader picks it up before looking for an installed platform
package. To use Lua 5.4 instead of LuaJIT, for example where LuaJIT does
not build, run:

```sh
yarn napi build --platform --release --js false --no-default-features --features lua54
```

## Examples

Runnable scripts are in [`examples/`](https://github.com/the-haus/tilemaker-rs/tree/main/crates/bindings/tilemaker-node/examples):

| Script | What it does |
|---|---|
| `geography.mjs` | Countries, regions, cities and towns from a planet. |
| `places-in-regions.mjs` | Each place's country and region, with a `PolygonIndex`. |
| `extract.mjs` | PBF extracts by tag filter and by region. |
| `parks-csv.mjs` | Areas and label points as CSV. |
| `pois-csv.mjs` | Points of interest as CSV. |
| `export-geojsonseq.mjs` | A GeoJSON lines export. |
| `tiles.mjs` | Vector tiles from a tilemaker profile. |

## Changelog

See [CHANGELOG.md](https://github.com/the-haus/tilemaker-rs/blob/main/CHANGELOG.md) for the release history.

## License

Released under the FTWPL, the same licence as
[tilemaker](https://github.com/systemed/tilemaker). The full text is in
[LICENCE.txt](https://github.com/the-haus/tilemaker-rs/blob/main/LICENCE.txt).
