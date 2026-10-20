# Architecture

## Reading PBF files (`tilemaker-pbf`)

`PbfFile::open` reads only the small blob headers to index the file (the
planet: 53,186 blocks in ~7 s). Blocks are then decoded by
`parallel::ordered_map`: a feeder thread reads blobs sequentially (good
for disks), N workers decompress (libdeflate for zlib; zstd, lz4 and raw
too) and decode them, and results come back in file order. A permit
channel caps how many blocks are in flight or waiting, so a slow consumer
slows the reader rather than buffering the file; a panic in a worker is
re-raised on the consumer.

A decoded `PrimitiveBlock` keeps its string table in one buffer (no
allocation per string), flat arrays of tag pairs, node refs and members,
and reuses scratch buffers while decoding (array sizes are known up front
from the varint terminators in packed fields). `Node`/`Way`/`Relation` are
borrowed views, so tags are never copied unless asked for.

For files sorted by type then id (`Sort.Type_then_ID`, what osmium
writes), `layout()` binary-searches the node/way/relation boundaries by
decoding a few dozen blocks, so a pass over ways never decompresses node
blocks. `first_element` inflates only the start of a block (its string
table and a few bytes more) to read its first element's type and id;
`blocks_with_ids` uses consecutive first ids to keep only blocks whose id
range holds a wanted id.

## Writing PBF files

`BlockBuilder` collects one block's elements with its own string table;
builders made on different threads merge (`append`, remapping string
indices). `ParallelWriter` takes builders in file order, merges small ones
up to 8,000 elements of one type, and hands full blocks to workers that
encode and compress them (libdeflate), writing them in order.

## Filters

`FilterSet` holds osmium `tags-filter` expressions. `for_block` resolves
them against one block's string table in a single pass (exact keys and
values by a length prefilter and comparison, wildcards by matching), only
for the filters that apply to the block's element types; matching an
element then compares string indices.

## Exporting (`tilemaker-export`)

Features need locations that live elsewhere in the file. The passes, each
skipped when it cannot select anything:

1. **relations scan** — selected area/line relations mark their member
   ways (`IdSetBuilder`, a Roaring-like set: 16-bit arrays per 65,536-id
   chunk, bitmaps when dense; ~2 bytes per id however sparse the ids);
   with `member_of`, every selected parent relation's memberships are
   collected too;
2. **ways scan** — selected ways and marked ways mark their nodes; when
   no way is selected for itself, the marked ways' node ids are kept
   (`RefStore`, varint deltas) so the ways need not be read again;
3. **nodes** — selected nodes become points; marked nodes' locations are
   stored (`NodeStore`: groups of 16 nodes, the first id and location in
   full, the rest as varint deltas of id, lat and lon, ~6 bytes a node; or
   a dense memory-mapped sparse file);
4. **ways** — selected ways become lines/polygons; marked ways' locations
   are stored (`WayStore`, varint deltas);
5. **relations** — areas are assembled from stored ways, in small chunks
   so one country does not hold up the rest.

Passes 2–4 read only the blocks that can hold a marked id when nothing is
selected for itself in them (relation-only selections), found by probing
first ids. Node lookups go through a cursor that keeps the last decoded
group, since a way's nodes usually have neighbouring ids.

Area assembly (`tilemaker_geom::assemble`) follows osmium: all way members
count once each; segments shared by two rings cancel; rings are walked
from a CSR adjacency over sorted endpoints and split wherever a location
repeats; crossing segments (an exact integer sweep) make the area invalid;
rings nest by containment, found through an R-tree and tested with a
banded edge index (O(√n) per point for million-point rings).

Every feature then goes through `emit`: the bbox test, the `Transform`
(clip, Douglas–Peucker, precision), the requested `Enrich` values
(spherical area and length, centroid, polylabel in Mercator space, bbox),
its parent relations (`member_of`, a sorted membership list) and the areas
it lies in (`within`: the selected areas are exported first, in their own
passes, into a `PolygonIndex`).

`export_features_with` maps each block's features on the worker that made
them; the GeoJSON writer (`geojson`) uses it to serialise in parallel,
printing coordinates from their 1e-7 fixed point, and writes the chunks in
file order.

## Extracts

`extract` runs osmium's two commands as passes over one file:

1. with a region: nodes inside (box test, or polygons with ring indexes);
   ways with a node inside, adding all their nodes; relations with a node
   inside or such a way as member, then their parents (a closure over
   relation-to-relation edges);
2. with filters: matching relations (within the region), then everything
   they reach through member relations (closure the other way), their
   member ways and nodes; then the nodes of matching and member ways;
3. a write pass decoding every block with metadata, keeping the selected
   elements in a `BlockBuilder` per block, on the workers, and merging
   them through the `ParallelWriter`.

## File statistics

`file_info` reads blob headers (and the first bytes of each blob, for its
compression); with `full`, every block is decoded with metadata on all
cores into per-block summaries (counts, id ranges, first and last ids,
bbox, timestamps, metadata attribute counts) merged in file order, which
also tells whether the file is ordered and has repeated elements.

## Polygon index

`PolygonIndex` keeps each polygon part's rings at 1e-7 degrees (8 bytes a
point) with a CSR index of the edges crossing each horizontal band (√n
bands), and the parts' boxes in an R-tree. A point test walks one band's
edges with exact integer arithmetic; lookups sort the containing features
by their (spherical) area. `smallest_many` splits points over threads.

## Node.js binding

Each export runs on its own thread and sends events through a napi
threadsafe function. Records cross as columnar batches (a few typed
arrays plus one string table per batch; computed values as extra columns,
parent relations and containing areas once per batch), which the
TypeScript layer turns into objects whose coordinates are subarray views.
Tasks without records (`writeGeoJSONSeq`, `extract`, `fileInfo`,
`writeTiles`) send only progress and their result.

Flow control is credit-based: the producer may send a batch only with a
credit, and JS grants one per batch it takes, so at most `readAhead`
batches are queued. The producer waits on its own thread, never on the
event loop or the libuv pool. The threadsafe function keeps the event
loop alive only while JS awaits a batch (or the end of a closed stream);
streams dropped unfinished are closed when collected (the native callback
holds the stream through a `WeakRef`).

Input bytes (`streamElements(bytes)`) flow the other way through a
channel with a byte budget: `push` reports when ~8 MB wait unread and a
`drain` event lets the JS pump continue.

`PolygonIndex` is filled from JS (one call per polygon, copying its typed
arrays), then built; `findMany` runs on the libuv pool against a shared
reference to the built index.

## Tiles (`tilemaker-core`)

The OSM file is read in the same pass structure as tilemaker, on a rayon
pool with one Lua state per thread (`tilemaker-lua`). Workers claim blocks
in file order and the results are merged in file order (at most a window
ahead), so object and attribute numbering — and with them the tiles — do
not depend on scheduling. Each Lua state reuses one `Object` (tags in one
buffer, `TagBuf`; node lists refilled in place); `Find` and `Holds` are
plain C functions comparing the key on the Lua stack with the current
tags, allocating nothing.

What tiles are made of is stored compactly:

- `OutputObject`s are 16 bytes (layer, kind, min zoom, z-order, attribute
  set, geometry); OSM ids are kept beside them only with `include_ids`;
- geometries live in a `GeometryStore`: a point in its 8-byte slot, lines
  and polygons as varint-delta records (`tilemaker_geom::packed`) in 64 MB
  pages, encoded on the workers; multi-part polygons record each part's
  box and byte length so parts outside a tile are skipped undecoded;
- attribute sets are stored back to back as 16-byte attributes with keys
  and string values interned (`AttributeStore`), deduplicated by hash;
- the node store is dropped after the ways pass: relation node members
  with a role (labels, admin centres) were stored apart for the relations
  pass.

Objects are indexed by the base-zoom tiles they touch (tilemaker's
supercover walk; large polygons by rectangle). Each zoom then collects its
tiles from the base index (sorted and grouped on the pool), and tiles are
rendered in parallel chunks — sorted by layer and z-order, merged,
clipped, scaled, simplified and encoded (`tilemaker-mvt`) — and handed out
in order to a sink (`tilemaker-output`: MBTiles, PMTiles, directory) or to
JS.
