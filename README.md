# tilemaker-rs

[![CI](https://github.com/the-haus/tilemaker-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/the-haus/tilemaker-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/the-haus/tilemaker-rs?include_prereleases)](https://github.com/the-haus/tilemaker-rs/releases)
[![npm](https://img.shields.io/npm/v/@tilemaker-rs/node.svg?label=npm)](https://www.npmjs.com/package/@tilemaker-rs/node)
[![Rust](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](#building-from-source)
[![License: FTWPL](https://img.shields.io/badge/license-FTWPL-lightgrey.svg)](LICENCE.txt)

**Vector tiles and streaming OpenStreetMap processing, in Rust.**

tilemaker-rs is a Rust port of [tilemaker](https://github.com/systemed/tilemaker).
It turns OpenStreetMap `.osm.pbf` files into vector tiles, using the same
`process.lua` profiles and `config.json` files. Around that pipeline it
adds a streaming toolkit for OSM data:

- filtered elements and assembled features;
- GeoJSON export with computed values;
- PBF extracts;
- reverse geocoding;
- file statistics.

Everything is available in three ways: the `tilemaker` command line tool,
a set of Rust crates, and a Node.js package
([`@tilemaker-rs/node`](crates/bindings/tilemaker-node)). Each is checked
against the tool it replaces, osmium or the C++ tilemaker, and is faster
and uses less memory.

## Contents

- [Features](#features)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Verified against the originals](#verified-against-the-originals)
- [Performance](#performance)
- [Project layout](#project-layout)
- [Building from source](#building-from-source)
- [Limitations](#limitations)
- [Documentation](#documentation)
- [Contributing and releases](#contributing-and-releases)
- [License](#license)

## Features

| | |
|---|---|
| **Vector tiles** | tilemaker-compatible: the same arguments, Lua profiles and configs; the OpenMapTiles profile runs unchanged. Writes MBTiles, PMTiles v3 or a directory of tiles. |
| **Feature export** | Points, lines and multipolygons assembled like `osmium export`, written as GeoJSON lines. Features can be clipped, simplified and rounded, and given computed values: area, length, centroid, label point, bounding box, parent relations, and the areas they lie in. |
| **Extracts** | PBF output identical to `osmium tags-filter` and `osmium extract`, by tag filter, bounding box, GeoJSON polygon or `.poly` file. |
| **Reverse geocoding** | A polygon index for point-in-polygon lookups, such as which country and region a place is in. |
| **File statistics** | `osmium fileinfo -e` equivalents, and an inspector for tilesets. |
| **Streaming by design** | Decoding runs in parallel on every core, in file order. Memory stays bounded: only what a selection needs is kept, in compressed stores, with an optional memory-mapped node store for planet-scale work. |

## Installation

### Command line

Prebuilt binaries for Linux (x64, arm64), macOS (Apple Silicon, Intel) and
Windows (x64) are attached to every
[GitHub release](https://github.com/the-haus/tilemaker-rs/releases), with
SHA-256 checksums:

```sh
VERSION=0.1.0
TARGET=aarch64-apple-darwin   # or x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu,
                              #    x86_64-apple-darwin, x86_64-pc-windows-msvc (.zip)
curl -fsSL "https://github.com/the-haus/tilemaker-rs/releases/download/v$VERSION/tilemaker-$VERSION-$TARGET.tar.gz" | tar xz
./tilemaker-$VERSION-$TARGET/tilemaker --version
```

Each archive also contains `resources/`, with ready-to-use profiles and
configs (OpenMapTiles, a small example, coastline and debug). To build the
CLI with Cargo instead:

```sh
cargo install --git https://github.com/the-haus/tilemaker-rs --tag v0.1.0 tilemaker-cli
```

### Node.js

```sh
npm install @tilemaker-rs/node
```

The package needs Node.js 18 or newer and has prebuilt binaries for Linux
(x64 and arm64, glibc and musl), macOS (x64 and arm64) and Windows (x64
and arm64). See the [package README](crates/bindings/tilemaker-node) for
details.

### Rust

The crates are not on crates.io yet; depend on them from Git:

```toml
[dependencies]
# filters, features, GeoJSON, extracts, polygon index
tilemaker-export = { git = "https://github.com/the-haus/tilemaker-rs", tag = "v0.1.0" }
# just the PBF reader and writer
tilemaker-pbf = { git = "https://github.com/the-haus/tilemaker-rs", tag = "v0.1.0" }
# the tile pipeline
tilemaker-core = { git = "https://github.com/the-haus/tilemaker-rs", tag = "v0.1.0" }
```

## Quick start

### Command line

```sh
# Vector tiles, with tilemaker-compatible arguments
tilemaker region.osm.pbf region.pmtiles --config config.json --process process.lua

# Filtered, assembled features as GeoJSON lines (like osmium tags-filter + export),
# with each city's country and region (reverse geocoding) and computed values
tilemaker export planet.osm.pbf -f n/place=city,town --within r/admin_level=2,4 -o places.geojsonseq
tilemaker export planet.osm.pbf -f r/admin_level=2 --area --label-point --simplify 0.001 -o countries.geojsonseq

# PBF extracts (like osmium tags-filter and osmium extract; identical output)
tilemaker extract planet.osm.pbf -f n/place=city,town -f r/admin_level=2,4 -o geography.osm.pbf
tilemaker extract netherlands.osm.pbf -b 4.7,52.3,5.0,52.45 -o amsterdam.osm.pbf

# What is in a PBF file, or a tileset
tilemaker fileinfo -e planet.osm.pbf
tilemaker inspect region.pmtiles
```

Run `tilemaker --help`, or `tilemaker <command> --help`, for every option.

### Node.js

```ts
import { openPbf, PolygonIndex } from "@tilemaker-rs/node";

const pbf = await openPbf("planet.osm.pbf");
for await (const f of pbf.features({ filters: ["r/admin_level=2"], enrich: { area: true } })) {
  // f.geometry.coordinates is a Float64Array view; nothing is buffered
}

const countries = await PolygonIndex.fromFile("planet.osm.pbf", { filters: ["r/admin_level=2"] });
countries.find(4.9, 52.37)?.tags.name; // "Nederland"
```

## Verified against the originals

- **PBF reading**: element counts equal `osmium fileinfo -e` on extracts,
  and the full planet (95 GB) decodes without errors.
- **Feature export**, compared with `osmium export` on the world's
  administrative boundaries and settlements (540 MB):
  - the same 1,605,283 points, with the same tags;
  - the same 249,381 boundary multipolygons, with identical polygon, ring
    and point counts;
  - identical areas for all but 3, where a self-touching ring is split at
    a different node and the area is still the same.
- **Extracts**, compared with `osmium tags-filter` and `osmium extract`:
  identical elements, tags and metadata (compared as OPL) on the
  Netherlands for a tag filter, a bounding box, a polygon with a hole, a
  `.poly` file and `-R`. The test suite compares Monaco when osmium is
  installed.
- **File statistics**: the same counts, id ranges, bounding box,
  timestamps and ordering as `osmium fileinfo -e`.
- **Tiles**, compared with the C++ tilemaker and the OpenMapTiles profile
  on Monaco, Liechtenstein and a 2.6 GB Netherlands extract (48 million
  features):
  - the same non-empty tiles;
  - feature counts within 0.01% overall, and within 1.5% per zoom and
    layer;
  - total tile bytes within 0.5%.

  These comparisons run in the test suite when the extracts are present
  (see [fixtures](fixtures/README.md)).

## Performance

Measured on an Apple M1 Max (10 cores, 32 GiB, macOS 26) against
osmium 1.19.1 and the C++ tilemaker 3.2.0. Times are wall clock; memory is
peak resident. The inputs are the Netherlands with its surroundings
(2.6 GB; 226 M nodes, 33 M ways, 373 k relations) and the full planet
(95 GB, 2026-10-12).

| Task | Input | tilemaker-rs | Reference tool |
|---|---|---|---|
| Vector tiles (OpenMapTiles profile) | Netherlands | **60.5 s** · 5.9 GB | C++ tilemaker: 90.3 s · 8.3 GB |
| Cities, towns and admin areas as GeoJSON | Netherlands | **2.1 s** · 128 MB | osmium tags-filter + export: 14.8 s · 2.2 GB |
| Cities, towns and admin areas as GeoJSON | Planet | **2 min 0 s** · 802 MB | osmium tags-filter + export: 10 min 27 s · 4.2 GB |
| Tag-filter extract | Netherlands | **5.0 s** · 144 MB | osmium tags-filter: 13.6 s · 2.2 GB |
| Tag-filter extract | Planet | **4 min 17 s** · 918 MB | osmium tags-filter: 7 min 4 s · 2.9 GB |
| Bounding-box extract (Amsterdam) | Netherlands | **6.8 s** · 202 MB | osmium extract: 9.6 s · 4.0 GB |
| File statistics (`fileinfo -e`) | Planet | **2 min 51 s** · 807 MB | osmium fileinfo: 3 min 1 s · 962 MB |

For the Node.js package compared with JavaScript PBF readers, see its
[README](crates/bindings/tilemaker-node#performance). The method, every
result and how to reproduce them are in [bench/](bench/README.md).

## Project layout

A Cargo workspace of small, focused crates, with the language bindings
under `crates/bindings`:

| Crate | Purpose |
|---|---|
| [`tilemaker-pbf`](crates/tilemaker-pbf) | OSM PBF reader and writer: blob index, parallel order-preserving block decoding with bounded memory, block probing, a sequential `BlobReader` for any `Read`, and a parallel writer. |
| [`tilemaker-geom`](crates/tilemaker-geom) | Projection and tile maths, clipping, simplification, ring building and multipolygon assembly, areas, lengths and centroids. |
| [`tilemaker-store`](crates/tilemaker-store) | Compressed id sets, node location stores (in memory or in a dense memory-mapped file) and the way geometry store. |
| [`tilemaker-export`](crates/tilemaker-export) | The streaming extractor: osmium-style filters, element and feature export, enrichments, the GeoJSON writer, PBF extracts, the polygon index and file statistics. |
| [`tilemaker-config`](crates/tilemaker-config) | `config.json` parsing. |
| [`tilemaker-lua`](crates/tilemaker-lua) | The `process.lua` API on mlua (LuaJIT, or Lua 5.4). |
| [`tilemaker-mvt`](crates/tilemaker-mvt) | Mapbox Vector Tile encoder and decoder. |
| [`tilemaker-output`](crates/tilemaker-output) | MBTiles, PMTiles v3 and directory writers. |
| [`tilemaker-core`](crates/tilemaker-core) | The tile pipeline: reading through the profile, the tile index and rendering. |
| [`tilemaker-cli`](crates/tilemaker-cli) | The `tilemaker` binary. |
| [`bindings/tilemaker-node`](crates/bindings/tilemaker-node) | The N-API binding, published as `@tilemaker-rs/node`. |

## Building from source

Requirements: Rust 1.87 or newer, a C compiler, and Node.js 18 or newer for
the Node binding. LuaJIT, SQLite, zstd and libdeflate are built from
vendored sources, so no system libraries are needed.

```sh
# The CLI
cargo build --release -p tilemaker-cli          # target/release/tilemaker

# Rust tests
cargo test --workspace --exclude tilemaker-node --release

# The Node binding
corepack enable && yarn install
cd crates/bindings/tilemaker-node
yarn build && yarn test
```

To use Lua 5.4 instead of LuaJIT (simpler to cross-compile), build with
`--no-default-features --features lua54`.

## Limitations

The following are not supported yet:

- **Shapefile and GeoJSON layers** in the tile config (`source`), and the
  Lua spatial queries that read them. `Intersects`, `FindIntersecting`,
  `CoveredBy`, `FindCovering` and `AreaIntersecting` report nothing.
- **Decluttering.** `declutter_below` is not applied, and `Score()` is
  accepted but ignored.
- **`--merge`** into existing MBTiles, and **`--store`** (on-disk stores
  for tile generation; the extractor has a memory-mapped node store).
- **Other input formats:** lzma- and bzip2-compressed PBF blobs (rare;
  zlib, zstd, lz4 and uncompressed blobs are supported); OSM XML, OPL,
  history and change files.
- **Some osmium options:** `tags-filter -i/-t/-e`, `extract` strategies
  other than `complete_ways`, and several extracts in one run.

See [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) for everything that
carries over from tilemaker.

## Documentation

| Document | Contents |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Passes, stores and flow control. |
| [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) | What carries over from tilemaker, and what differs. |
| [Node.js package](crates/bindings/tilemaker-node) | The full JavaScript and TypeScript API. |
| [Benchmarks](bench/README.md) | Method, results and how to reproduce them. |
| [Changelog](CHANGELOG.md) | Release history. |
| [tilemaker configuration](https://github.com/systemed/tilemaker/blob/master/docs/CONFIGURATION.md) | Writing `config.json` and `process.lua` (upstream documentation). |

## Contributing and releases

Development setup, conventions and the release process are described in
[CONTRIBUTING.md](CONTRIBUTING.md). Releases are cut from a `vX.Y.Z` tag.
A single workflow then publishes all of the following:

- the npm packages;
- the CLI binaries;
- a GitHub release with notes and checksums.

## License

Released under the FTWPL, the same licence as tilemaker. See
[LICENCE.txt](LICENCE.txt).
