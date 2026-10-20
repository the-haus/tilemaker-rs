/**
 * @tilemaker-rs/node — stream OpenStreetMap data out of `.osm.pbf` files.
 *
 * ```ts
 * import { openPbf, toGeoJSON } from "@tilemaker-rs/node";
 *
 * const pbf = await openPbf("planet.osm.pbf");
 * for await (const f of pbf.features({ filters: ["n/place=city,town", "r/admin_level=2,4"] })) {
 *   // f.type, f.id, f.tags, f.geometry (flat Float64Array coordinates)
 * }
 * ```
 *
 * Decoding runs on native threads on all cores; the JS thread only turns
 * finished batches into objects. Memory is bounded: only the node and way
 * locations the selection needs are kept (compressed), and the producer
 * never gets more than `readAhead` batches ahead of the consumer.
 *
 * It also makes vector tiles with tilemaker profiles: {@link generateTiles}
 * streams them, {@link writeTiles} writes MBTiles, PMTiles or directories.
 */

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { decodeElements, decodeFeatures, decodeTiles } from "./decode.js";
import * as native from "./native.js";
import type {
  NativeExtractOptions,
  NativeReader,
  NativeStream,
  NativeStreamOptions,
  NativeTileOptions,
} from "./native.js";
import { OsmStream } from "./stream.js";
import {
  OsmStreamError,
  type CommonOptions,
  type Element,
  type ElementOptions,
  type ExtractOptions,
  type ExtractStats,
  type Feature,
  type FileInfo,
  type FileInfoOptions,
  type FeatureOptions,
  type PbfHeader,
  type Tile,
  type TileOptions,
  type TileProgress,
  type TileStats,
  type TileStreamOptions,
  type WriteGeoJSONSeqOptions,
  type WriteTilesOptions,
  type ExportStats,
} from "./types.js";

export * from "./types.js";
export { OsmStream } from "./stream.js";
export { PolygonIndex, type PolygonEntry } from "./polygon-index.js";
export {
  bbox,
  geoJSONSeq,
  geometryToGeoJSON,
  polygonRings,
  toGeoJSON,
  type GeoJSONFeature,
  type GeoJSONGeometry,
  type Position,
} from "./geojson.js";

function nativeOptions(
  o: CommonOptions & FeatureOptions & ElementOptions & Partial<Pick<WriteGeoJSONSeqOptions, "recordSeparator">>,
): NativeStreamOptions {
  const out: NativeStreamOptions = {};
  if (o.filters !== undefined) out.filters = [...o.filters];
  if (o.tags !== undefined) out.tags = Array.isArray(o.tags) ? [...o.tags] : o.tags;
  if (o.metadata !== undefined) out.metadata = o.metadata;
  if (o.bbox !== undefined) out.bbox = [...o.bbox];
  if (o.threads !== undefined) out.threads = o.threads;
  if (o.nodeStoreFile !== undefined) out.nodeStoreFile = o.nodeStoreFile;
  if (o.batchSize !== undefined) out.batchSize = o.batchSize;
  if (o.readAhead !== undefined) out.readAhead = o.readAhead;
  if (o.geometryTypes !== undefined) out.geometryTypes = [...o.geometryTypes];
  if (o.closedWays !== undefined) out.closedWays = o.closedWays;
  if (o.relationLines !== undefined) out.relationLines = o.relationLines;
  if (o.types !== undefined) out.types = [...o.types];
  if (o.wayLocations !== undefined) out.wayLocations = o.wayLocations;
  if (o.includeUntagged !== undefined) out.includeUntagged = o.includeUntagged;
  if (o.clip !== undefined) out.clip = [...o.clip];
  if (o.simplify !== undefined) out.simplify = o.simplify;
  if (o.precision !== undefined) out.precision = o.precision;
  if (o.enrich !== undefined) {
    const e = o.enrich;
    out.enrich = {};
    if (e.area !== undefined) out.enrich.area = e.area;
    if (e.length !== undefined) out.enrich.length = e.length;
    if (e.centroid !== undefined) out.enrich.centroid = e.centroid;
    if (e.labelPoint !== undefined) out.enrich.labelPoint = e.labelPoint;
    if (e.bbox !== undefined) out.enrich.bbox = e.bbox;
    if (e.memberOf !== undefined) out.enrich.memberOf = Array.isArray(e.memberOf) ? [...e.memberOf] : e.memberOf;
    if (e.within !== undefined) out.enrich.within = [...e.within];
  }
  if (o.recordSeparator !== undefined) out.recordSeparator = o.recordSeparator;
  return out;
}

const toPath = (p: string | URL): string => (p instanceof URL ? fileURLToPath(p) : p);

async function nativeTileOptions(o: TileOptions): Promise<NativeTileOptions> {
  const configJson =
    typeof o.config === "string" || o.config instanceof URL
      ? await readFile(toPath(o.config), "utf8")
      : JSON.stringify(o.config);
  let processSource: string;
  let processName: string;
  if (typeof o.process === "string" || o.process instanceof URL) {
    processName = toPath(o.process);
    processSource = await readFile(processName, "utf8");
  } else {
    processSource = o.process.source;
    processName = o.process.name ?? "process.lua";
  }
  const out: NativeTileOptions = { configJson, processSource, processName };
  if (o.bbox !== undefined) out.bbox = [...o.bbox];
  if (o.threads !== undefined) out.threads = o.threads;
  return out;
}

function emptyTileStats(stopped: boolean): TileStats {
  return {
    read: {
      nodesProcessed: 0,
      waysProcessed: 0,
      relationsProcessed: 0,
      relationsAccepted: 0,
      waysMissingNodes: 0,
      objects: 0,
      storedNodes: 0,
      storedWays: 0,
      geometryBytes: 0,
      attributeSets: 0,
    },
    tiles: 0,
    tilesPerZoom: [],
    bytes: 0,
    warnings: [],
    stopped,
    elapsedMs: 0,
    metadata: {
      name: "",
      version: "",
      description: "",
      minzoom: 0,
      maxzoom: 0,
      bounds: null,
      center: null,
      vector_layers: [],
    },
  };
}

/** An open, indexed PBF file. Create one with {@link openPbf}. */
export class PbfReader {
  readonly #native: NativeReader;

  /** @internal */
  constructor(n: NativeReader) {
    this.#native = n;
  }

  /** The file header. */
  get header(): PbfHeader {
    const h = this.#native.header;
    return {
      bbox: h.bbox && h.bbox.length === 4 ? [h.bbox[0]!, h.bbox[1]!, h.bbox[2]!, h.bbox[3]!] : null,
      requiredFeatures: h.requiredFeatures,
      optionalFeatures: h.optionalFeatures,
      writingProgram: h.writingProgram ?? null,
      source: h.source ?? null,
      replicationTimestamp: h.replicationTimestamp ?? null,
      replicationSequenceNumber: h.replicationSequenceNumber ?? null,
      replicationBaseUrl: h.replicationBaseUrl ?? null,
      sorted: h.sorted,
      locationsOnWays: h.locationsOnWays,
    };
  }

  /** Number of data blocks. */
  get blockCount(): number {
    return this.#native.blockCount;
  }

  /** File size in bytes. */
  get size(): number {
    return this.#native.size;
  }

  /** The file's path. */
  get path(): string {
    return this.#native.path;
  }

  /**
   * Stream assembled features: points from nodes, linestrings and polygons
   * from ways, multipolygons from `type=multipolygon|boundary` relations
   * (and multi-linestrings from other relations with `relationLines`).
   */
  features(options: FeatureOptions = {}): OsmStream<Feature> {
    const opts = nativeOptions(options);
    return new OsmStream<Feature>(
      (cb) => this.#native.features(opts, cb),
      decodeFeatures as (b: never) => Feature[],
      options.onProgress,
      options.signal,
    );
  }

  /**
   * Make vector tiles with a tilemaker profile and config, streaming them
   * as they are finished (zoom by zoom, then by x and y). The OSM data is
   * read first, so the first tile arrives after the reading passes.
   */
  tiles(options: TileStreamOptions): OsmStream<Tile, TileStats, TileProgress> {
    return tileStream(async () => this.#native, options);
  }

  /** Make vector tiles into an `.mbtiles`, `.pmtiles` or a directory. */
  writeTiles(options: WriteTilesOptions): Promise<TileStats> {
    return writeTilesWith(async () => this.#native, options);
  }

  /**
   * Write features as newline-delimited GeoJSON (see {@link writeGeoJSONSeq}).
   * Resolves with the export's counters.
   */
  writeGeoJSONSeq(options: WriteGeoJSONSeqOptions): Promise<ExportStats> {
    return writeGeoJSONSeqWith(async () => this.#native, options);
  }

  /** Write an extract as PBF (see {@link extract}). */
  extract(options: ExtractOptions): Promise<ExtractStats> {
    return extractWith(async () => this.#native, options);
  }

  /** What the file holds (see {@link fileInfo}). */
  fileInfo(options: FileInfoOptions = {}): Promise<FileInfo> {
    return fileInfoWith(async () => this.#native, options);
  }

  /** Stream raw elements: nodes, ways (node ids, optionally coordinates) and relations (members). */
  elements(options: ElementOptions = {}): OsmStream<Element> {
    const opts = nativeOptions(options);
    return new OsmStream<Element>(
      (cb) => this.#native.elements(opts, cb),
      decodeElements as (b: never) => Element[],
      options.onProgress,
      options.signal,
    );
  }
}

/**
 * Open a PBF file. Indexing reads only the small blob headers (the planet
 * takes a few seconds), on the libuv pool.
 */
export async function openPbf(path: string | URL): Promise<PbfReader> {
  const p = toPath(path);
  try {
    return new PbfReader(await native.open(p));
  } catch (e) {
    throw new OsmStreamError(e instanceof Error ? e.message : String(e));
  }
}

/** {@link PbfReader.features} on a path, opening the file when iteration starts. */
export function streamFeatures(path: string | URL, options: FeatureOptions = {}): OsmStream<Feature> {
  const opts = nativeOptions(options);
  return new OsmStream<Feature>(
    async (cb) => {
      const r = await openNative(path);
      return r.features(opts, cb);
    },
    decodeFeatures as (b: never) => Feature[],
    options.onProgress,
    options.signal,
  );
}

/** Bytes of a PBF file arriving over time. */
export type ByteSource = AsyncIterable<Uint8Array> | Iterable<Uint8Array> | ReadableStream<Uint8Array> | Uint8Array;

/**
 * Elements from a path (as {@link PbfReader.elements}), or from bytes as
 * they arrive — a `fetch()` body, a Node `Readable`, stdin, any (async)
 * iterable of `Uint8Array`s — without saving the file first. Reading from
 * bytes is one pass, so `wayLocations` then needs a file with
 * `LocationsOnWays`; progress reports `blocksTotal: 0` (unknown).
 *
 * ```ts
 * const res = await fetch("https://download.geofabrik.de/europe/monaco-latest.osm.pbf");
 * for await (const e of streamElements(res.body!, { filters: ["amenity"] })) { … }
 * ```
 */
export function streamElements(source: string | URL | ByteSource, options: ElementOptions = {}): OsmStream<Element> {
  const opts = nativeOptions(options);
  if (typeof source === "string" || source instanceof URL) {
    return new OsmStream<Element>(
      async (cb) => {
        const r = await openNative(source);
        return r.elements(opts, cb);
      },
      decodeElements as (b: never) => Element[],
      options.onProgress,
      options.signal,
    );
  }
  let stream: OsmStream<Element>;
  const pump = async (n: NativeStream): Promise<void> => {
    try {
      for await (const chunk of iterateBytes(source)) {
        if (stream._finished) break;
        const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength);
        const status = n.push(buf);
        if (status === 2) break;
        if (status === 1) await stream._drained();
      }
      n.endInput();
    } catch (e) {
      n.endInput();
      stream._fail(e instanceof Error ? e : new OsmStreamError(String(e)));
    }
  };
  stream = new OsmStream<Element>(
    (cb) => {
      const n = native.elementsFromInput(opts, cb);
      void pump(n);
      return n;
    },
    decodeElements as (b: never) => Element[],
    options.onProgress,
    options.signal,
  );
  return stream;
}

async function* iterateBytes(source: ByteSource): AsyncGenerator<Uint8Array, void, undefined> {
  const web = source as unknown as ReadableStream<Uint8Array>;
  if (typeof web.getReader === "function" && !(Symbol.asyncIterator in source)) {
    const reader = web.getReader();
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) return;
        yield value;
      }
    } finally {
      reader.releaseLock();
    }
  }
  if ((source as unknown) instanceof Uint8Array) {
    yield source as unknown as Uint8Array;
    return;
  }
  yield* source as AsyncIterable<Uint8Array>;
}

function tileStream(
  reader: () => Promise<NativeReader>,
  options: TileStreamOptions,
): OsmStream<Tile, TileStats, TileProgress> {
  return new OsmStream<Tile, TileStats, TileProgress>(
    async (cb) => {
      const o = await nativeTileOptions(options);
      if (options.compression !== undefined) o.compression = options.compression;
      if (options.batchSize !== undefined) o.batchSize = options.batchSize;
      if (options.readAhead !== undefined) o.readAhead = options.readAhead;
      return (await reader()).tiles(o, cb);
    },
    decodeTiles as (b: never) => Tile[],
    options.onProgress,
    options.signal,
    emptyTileStats,
  );
}

async function writeTilesWith(reader: () => Promise<NativeReader>, options: WriteTilesOptions): Promise<TileStats> {
  // A stream that produces no records: only progress and the end.
  const stream = new OsmStream<never, TileStats, TileProgress>(
    async (cb) => {
      const o = await nativeTileOptions(options);
      o.output = toPath(options.output);
      return (await reader()).writeTiles(o, cb);
    },
    () => [],
    options.onProgress,
    options.signal,
    emptyTileStats,
  );
  for await (const _ of stream) {
    void _;
  }
  return stream.stats;
}

async function writeGeoJSONSeqWith(
  reader: () => Promise<NativeReader>,
  options: WriteGeoJSONSeqOptions,
): Promise<ExportStats> {
  const opts = nativeOptions(options);
  opts.output = toPath(options.output);
  const stream = new OsmStream<never>(
    async (cb) => (await reader()).writeGeoJSONSeq(opts, cb),
    () => [],
    options.onProgress,
    options.signal,
  );
  for await (const _ of stream) {
    void _;
  }
  return stream.stats;
}

/**
 * Export features from `input` straight into a newline-delimited GeoJSON
 * file (`options.output`): filtering, assembly, serialisation and writing
 * all happen on native threads, so no feature crosses into JavaScript —
 * the fastest way to make a `.geojsonseq`. Each line is the
 * {@link toGeoJSON} of a feature. Takes every feature option (filters,
 * `clip`, `simplify`, `precision`, `enrich`, …).
 *
 * ```ts
 * const stats = await writeGeoJSONSeq("planet.osm.pbf", {
 *   filters: ["r/admin_level=2"],
 *   enrich: { area: true, labelPoint: true },
 *   output: "countries.geojsonseq",
 * });
 * ```
 */
export function writeGeoJSONSeq(input: string | URL, options: WriteGeoJSONSeqOptions): Promise<ExportStats> {
  return writeGeoJSONSeqWith(() => openNative(input), options);
}

async function extractWith(reader: () => Promise<NativeReader>, options: ExtractOptions): Promise<ExtractStats> {
  const opts: NativeExtractOptions = { output: toPath(options.output) };
  if (options.filters !== undefined) opts.filters = [...options.filters];
  if (options.bbox !== undefined) opts.bbox = [...options.bbox];
  if (options.polygon !== undefined) {
    const p = options.polygon;
    if (typeof p === "string" || p instanceof URL) opts.polygonFile = toPath(p);
    else opts.polygonJson = JSON.stringify(p);
  }
  if (options.omitReferenced !== undefined) opts.omitReferenced = options.omitReferenced;
  if (options.metadata !== undefined) opts.metadata = options.metadata;
  if (options.threads !== undefined) opts.threads = options.threads;
  const stream = new OsmStream<never, ExtractStats>(
    async (cb) => (await reader()).extract(opts, cb),
    () => [],
    options.onProgress,
    options.signal,
  );
  for await (const _ of stream) {
    void _;
  }
  return stream.stats;
}

/**
 * Write an extract of `input` as a PBF file, as `osmium extract` and
 * `osmium tags-filter` make them, in one run on all cores:
 *
 * - a region (`bbox` or `polygon`) keeps the nodes inside, the ways with a
 *   node inside (with all their nodes), the relations with such a member,
 *   and their parent relations (osmium's `complete_ways` strategy);
 * - `filters` then keep the matching elements with the nodes and members
 *   they reference (recursively), unless `omitReferenced`.
 *
 * Tags and metadata are kept; the output is sorted when the input is, and
 * is identical to osmium's.
 *
 * ```ts
 * await extract("planet.osm.pbf", {
 *   output: "geography.osm.pbf",
 *   filters: ["n/place=city,town", "r/admin_level=2,4"],
 * });
 * ```
 */
export function extract(input: string | URL, options: ExtractOptions): Promise<ExtractStats> {
  return extractWith(() => openNative(input), options);
}

async function fileInfoWith(reader: () => Promise<NativeReader>, options: FileInfoOptions): Promise<FileInfo> {
  const opts: { extended?: boolean; threads?: number } = {};
  if (options.extended !== undefined) opts.extended = options.extended;
  if (options.threads !== undefined) opts.threads = options.threads;
  const stream = new OsmStream<never, FileInfo>(
    async (cb) => (await reader()).fileInfo(opts, cb),
    () => [],
    options.onProgress,
    options.signal,
  );
  for await (const _ of stream) {
    void _;
  }
  return stream.stats;
}

/**
 * What a PBF file holds, like `osmium fileinfo -e`: its header, size,
 * blocks and their compression; with `extended: true` it reads every
 * element (on all cores) for counts, id ranges, the data's bounding box,
 * timestamps, ordering and metadata.
 */
export function fileInfo(path: string | URL, options: FileInfoOptions = {}): Promise<FileInfo> {
  return fileInfoWith(() => openNative(path), options);
}

/** {@link PbfReader.tiles} on a path. */
export function generateTiles(input: string | URL, options: TileStreamOptions): OsmStream<Tile, TileStats, TileProgress> {
  return tileStream(() => openNative(input), options);
}

/** {@link PbfReader.writeTiles} on a path. */
export function writeTiles(input: string | URL, options: WriteTilesOptions): Promise<TileStats> {
  return writeTilesWith(() => openNative(input), options);
}

async function openNative(path: string | URL): Promise<NativeReader> {
  return native.open(toPath(path));
}

/** The native library's version. */
export function version(): string {
  return native.version();
}
