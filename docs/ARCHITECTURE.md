# Architecture

## Reading PBF files (`tilemaker-pbf`)

`PbfFile::open` reads only the small blob headers to index the file (the
planet: 53,186 blocks in ~7 s). Blocks are then decoded by
`parallel::ordered_map`: a feeder thread reads blobs sequentially (good
for disks), N workers decompress and decode them, and results come back
in file order. A permit channel caps how many blocks are in flight or
waiting, so a slow consumer slows the reader rather than buffering the
file; a panic in a worker is re-raised on the consumer.

A decoded `PrimitiveBlock` keeps one string table and flat arrays of tag
pairs, node refs and members; `Node`/`Way`/`Relation` are borrowed views,
so tags are never copied unless asked for.

For files sorted by type then id (`Sort.Type_then_ID`, what osmium
writes), `layout()` binary-searches the node/way/relation boundaries by
decoding a few dozen blocks, so a pass over ways never decompresses node
blocks.

## Exporting (`tilemaker-export`)

Features need locations that live elsewhere in the file. The passes, each
skipped when it cannot select anything:

1. **relations scan** — selected area/line relations mark their member
   ways (`IdSetBuilder`, a Roaring-like set: 16-bit arrays per 65,536-id
   chunk, bitmaps when dense; ~2 bytes per id however sparse the ids);
2. **ways scan** — selected ways and marked ways mark their nodes; when
   no way is selected for itself, the marked ways' node ids are kept
   (`RefStore`, varint deltas) so the ways need not be read again;
3. **nodes** — selected nodes become points; marked nodes' locations are
   stored (`NodeStore`: groups of 64 nodes, one full id plus varint
   deltas, ~10 bytes a node; or a dense memory-mapped sparse file);
4. **ways** — selected ways become lines/polygons; marked ways' locations
   are stored (`WayStore`, varint deltas);
5. **relations** — areas are assembled from stored ways, in small chunks
   so one country does not hold up the rest.

Area assembly (`tilemaker_geom::assemble`) follows osmium: all way members
count once each; segments shared by two rings cancel; rings are walked
from a CSR adjacency over sorted endpoints and split wherever a location
repeats; crossing segments (an exact integer sweep) make the area invalid;
rings nest by containment, found through an R-tree and tested with a
banded edge index (O(√n) per point for million-point rings).

## Node.js binding

Each export runs on its own thread and sends events through a napi
threadsafe function. Records cross as columnar batches (a few typed
arrays plus one string table per batch), which the TypeScript layer turns
into objects whose coordinates are subarray views.

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

## Tiles (`tilemaker-core`)

The OSM file is read in the same pass structure as tilemaker, on a rayon
pool with one Lua state per thread (`tilemaker-lua`). Each object's
outputs are stored compactly (fixed-point flat arrays) and indexed by the
base-zoom tiles they touch (tilemaker's supercover walk; large polygons by
rectangle). Each zoom then collects its tiles from the base index, and
tiles are rendered in parallel chunks — sorted by layer and z-order,
merged, clipped, scaled, simplified and encoded (`tilemaker-mvt`) — and
handed out in order to a sink (`tilemaker-output`: MBTiles, PMTiles,
directory) or to JS.
