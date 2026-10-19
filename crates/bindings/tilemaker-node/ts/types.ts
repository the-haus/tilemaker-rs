/** An OpenStreetMap element type. */
export type OsmType = "node" | "way" | "relation";

/** Tags of an element. */
export type Tags = Record<string, string>;

/** Element metadata (when `metadata: true`). */
export interface Meta {
  version: number;
  /** Seconds since the Unix epoch. */
  timestamp: number;
  changeset: number;
  uid: number;
  user: string;
  /** `false` only for deleted versions in history files. */
  visible: boolean;
}

/**
 * Geometries keep their coordinates flat: `[lon, lat, lon, lat, …]` in a
 * `Float64Array` — a zero-copy view into the batch that carried it, so
 * holding on to one feature keeps its whole batch's buffer alive. To keep a
 * few features out of many, copy them (`new Float64Array(g.coordinates)`).
 * Use {@link toGeoJSON} / {@link geometryToGeoJSON} for nested arrays.
 */
export interface PointGeometry {
  type: "Point";
  /** `[lon, lat]`. */
  coordinates: Float64Array;
}

export interface LineStringGeometry {
  type: "LineString";
  coordinates: Float64Array;
}

export interface MultiLineStringGeometry {
  type: "MultiLineString";
  coordinates: Float64Array;
  /** Exclusive end of each line, in points. */
  parts: Uint32Array;
}

export interface MultiPolygonGeometry {
  type: "MultiPolygon";
  coordinates: Float64Array;
  /** Exclusive end of each ring, in points. */
  parts: Uint32Array;
  /**
   * Exclusive end of each polygon, in rings. A polygon's first ring is the
   * exterior (counter-clockwise), the others are holes (clockwise).
   */
  polygons: Uint32Array;
}

export type Geometry = PointGeometry | LineStringGeometry | MultiLineStringGeometry | MultiPolygonGeometry;

export type GeometryType = Geometry["type"];

/** An assembled feature. */
export interface Feature<G extends Geometry = Geometry> {
  /** The element it was made from. */
  type: OsmType;
  id: number;
  tags: Tags;
  geometry: G;
  meta?: Meta;
}

export interface NodeElement {
  type: "node";
  id: number;
  tags: Tags;
  lon: number;
  lat: number;
  meta?: Meta;
}

export interface WayElement {
  type: "way";
  id: number;
  tags: Tags;
  /** Node ids. */
  refs: Float64Array;
  /**
   * With `wayLocations: true`: `[lon, lat, …]` per node; `NaN, NaN` where a
   * node is missing from the file.
   */
  coordinates?: Float64Array;
  meta?: Meta;
}

export interface Member {
  type: OsmType;
  ref: number;
  role: string;
}

export interface RelationElement {
  type: "relation";
  id: number;
  tags: Tags;
  members: Member[];
  meta?: Meta;
}

export type Element = NodeElement | WayElement | RelationElement;

/** A pass over the file (see the README's "How it works"). */
export type Phase = "scanRelations" | "scanWays" | "nodes" | "ways" | "relations" | "elements";

export interface Progress {
  phase: Phase;
  blocksDone: number;
  blocksTotal: number;
}

/** Progress of tile generation: reading passes, then tiles zoom by zoom. */
export type TileProgress =
  | { phase: Exclude<Phase, "elements">; blocksDone: number; blocksTotal: number }
  | { phase: "tiles"; zoom: number; tilesDone: number; tilesTotal: number };

/** A vector tile. */
export interface Tile {
  z: number;
  x: number;
  /** Row in the XYZ scheme (0 at the top). */
  y: number;
  /** Mapbox Vector Tile bytes, compressed as requested (default: none). */
  data: Buffer;
}

/** Options for tile generation (see tilemaker's CONFIGURATION.md). */
export interface TileOptions {
  /**
   * The config: a path to a `config.json`, or its parsed contents.
   * Layers with a shapefile/GeoJSON `source` are not supported (they stay empty).
   */
  config: string | URL | Record<string, unknown>;
  /** The Lua profile: a path to a `process.lua`, or `{ source }` with its code. */
  process: string | URL | { source: string; name?: string };
  /** Only tiles intersecting `[west, south, east, north]` (default: the file header's box). */
  bbox?: [number, number, number, number];
  /** Worker threads (default: all cores). */
  threads?: number;
  /** Called with progress, at most every 200 ms and at phase changes. */
  onProgress?: (progress: TileProgress) => void;
  /** Abort generation. */
  signal?: AbortSignal;
}

export interface TileStreamOptions extends TileOptions {
  /** Compress tile data: `"none"` (default), `"gzip"`, `"deflate"`, or `"config"` (the config's `compress`). */
  compression?: "none" | "gzip" | "deflate" | "config";
  /** Tiles per batch (default 64). */
  batchSize?: number;
  /** Batches generated ahead of the consumer (default 2). */
  readAhead?: number;
}

export interface WriteTilesOptions extends TileOptions {
  /** `.mbtiles`, `.pmtiles`, or a directory (`z/x/y.pbf`); an existing file is replaced. */
  output: string | URL;
}

/** Counters from tile generation. */
export interface TileStats {
  read: {
    nodesProcessed: number;
    waysProcessed: number;
    relationsProcessed: number;
    relationsAccepted: number;
    waysMissingNodes: number;
    objects: number;
    storedNodes: number;
    storedWays: number;
  };
  tiles: number;
  /** Tiles per zoom (index = zoom). */
  tilesPerZoom: number[];
  /** Uncompressed MVT bytes. */
  bytes: number;
  /** Problems the profile ran into (first 100). */
  warnings: string[];
  stopped: boolean;
  elapsedMs: number;
  /** The tileset metadata (as written to MBTiles/PMTiles). */
  metadata: {
    name: string;
    version: string;
    description: string;
    minzoom: number;
    maxzoom: number;
    bounds: [number, number, number, number] | null;
    center: [number, number, number] | null;
    vector_layers: Array<{ id: string; fields: Record<string, string>; minzoom: number; maxzoom: number }>;
  };
}

/** Counters reported when a stream ends. */
export interface ExportStats {
  emitted: number;
  points: number;
  lines: number;
  multiLines: number;
  polygons: number;
  /** Ways skipped because a node was missing from the file. */
  waysMissingNodes: number;
  /** Relations skipped because member ways were missing from the file. */
  relationsMissingWays: number;
  /** Area relations whose ways did not close into rings. */
  invalidAreas: number;
  storedNodes: number;
  storedWays: number;
  /** Approximate peak bytes held by the location stores. */
  storeBytes: number;
  /** Whether the stream was closed before the end. */
  stopped: boolean;
  elapsedMs: number;
}

export interface PbfHeader {
  /** `[west, south, east, north]`, when the file records one. */
  bbox: [number, number, number, number] | null;
  requiredFeatures: string[];
  optionalFeatures: string[];
  writingProgram: string | null;
  source: string | null;
  replicationTimestamp: number | null;
  replicationSequenceNumber: number | null;
  replicationBaseUrl: string | null;
  /** Elements are sorted by type, then id (enables skipping blocks). */
  sorted: boolean;
  /** Ways carry their node locations. */
  locationsOnWays: boolean;
}

/** Options shared by {@link PbfReader.features} and {@link PbfReader.elements}. */
export interface CommonOptions {
  /**
   * osmium `tags-filter` expressions; an element is kept when any matches.
   * `[TYPES/]KEY`, `KEY=V1,V2`, `KEY!=V`, `!KEY`, with `TYPES` from `n`, `w`,
   * `r`, `a` (areas) and `*` wildcards in keys and values. Default: every
   * tagged element.
   *
   * @example ["n/place=city,town", "r/admin_level=2,4"]
   */
  filters?: string[];
  /** `true` (default): all tags; `false`: none; a list: only these keys (`*` wildcards). */
  tags?: boolean | string[];
  /** Include version, timestamp, changeset, uid and user. */
  metadata?: boolean;
  /**
   * Only what intersects `[west, south, east, north]`. For elements: nodes,
   * and ways whose coordinates are known (`wayLocations`, or files with
   * `LocationsOnWays`); relations are not filtered.
   */
  bbox?: [number, number, number, number];
  /** Decoding threads (default: all cores). */
  threads?: number;
  /**
   * Keep node locations in a dense on-disk (sparse, memory-mapped) file at
   * this path instead of compressed in memory: for planet-scale selections
   * that need hundreds of millions of nodes. The file must not exist yet; it is
   * removed (on Unix, unlinked as soon as it is mapped).
   */
  nodeStoreFile?: string;
  /** Records per batch (default 4096). */
  batchSize?: number;
  /** Batches decoded ahead of the consumer (default 2). Bounds memory. */
  readAhead?: number;
  /** Called with progress, at most every 200 ms and at phase changes. */
  onProgress?: (progress: Progress) => void;
  /** Abort the stream. */
  signal?: AbortSignal;
}

export interface FeatureOptions extends CommonOptions {
  /** Geometry types to produce (default all). */
  geometryTypes?: Array<"point" | "linestring" | "polygon">;
  /**
   * What closed ways become: `"auto"` (default: polygons when tagged like an
   * area, e.g. `building`, `landuse`, `area=yes`; else linestrings),
   * `"polygon"`, `"linestring"` or `"both"` (like `osmium export`).
   */
  closedWays?: "auto" | "polygon" | "linestring" | "both";
  /** Also turn non-area relations (routes, waterways, …) into multi-linestrings. */
  relationLines?: boolean;
}

export interface ElementOptions extends CommonOptions {
  /** Element types to emit (default all). */
  types?: OsmType[];
  /** Give ways their node coordinates (adds passes unless the file has them). */
  wayLocations?: boolean;
  /** With no filters, also emit untagged elements. */
  includeUntagged?: boolean;
}

/** The error thrown for invalid input or a failed export. */
export class OsmStreamError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "OsmStreamError";
  }
}
