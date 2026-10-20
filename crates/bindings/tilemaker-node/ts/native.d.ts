// Types of the N-API module (see src/lib.rs). The public, documented API
// is in index.ts; these are internal.

export interface NativeStreamOptions {
  filters?: string[];
  tags?: boolean | string[];
  metadata?: boolean;
  bbox?: number[];
  threads?: number;
  nodeStoreFile?: string;
  batchSize?: number;
  readAhead?: number;
  geometryTypes?: string[];
  closedWays?: string;
  relationLines?: boolean;
  types?: string[];
  wayLocations?: boolean;
  includeUntagged?: boolean;
  clip?: number[];
  simplify?: number;
  precision?: number;
  enrich?: {
    area?: boolean;
    length?: boolean;
    centroid?: boolean;
    labelPoint?: boolean;
    bbox?: boolean;
    memberOf?: boolean | string[];
    within?: string[];
  };
  output?: string;
  recordSeparator?: boolean;
}

export interface NativeHeader {
  bbox?: number[] | null;
  requiredFeatures: string[];
  optionalFeatures: string[];
  writingProgram?: string | null;
  source?: string | null;
  replicationTimestamp?: number | null;
  replicationSequenceNumber?: number | null;
  replicationBaseUrl?: string | null;
  sorted: boolean;
  locationsOnWays: boolean;
}

export interface NativeMeta {
  present: Uint8Array;
  version: Int32Array;
  timestamp: Float64Array;
  changeset: Float64Array;
  uid: Int32Array;
  user: Uint32Array;
  visible: Uint8Array;
}

export interface NativeFeatureBatch {
  kind: "features";
  strings: string[];
  types: Uint8Array;
  ids: Float64Array;
  tagOffsets: Uint32Array;
  tags: Uint32Array;
  geometryTypes: Uint8Array;
  coordOffsets: Uint32Array;
  coords: Float64Array;
  partOffsets: Uint32Array;
  parts: Uint32Array;
  polygonOffsets: Uint32Array;
  polygons: Uint32Array;
  meta?: NativeMeta;
  area?: Float64Array;
  length?: Float64Array;
  centroid?: Float64Array;
  labelPoint?: Float64Array;
  bbox?: Float64Array;
  memberOf?: {
    offsets: Uint32Array;
    relations: Uint32Array;
    roles: Uint32Array;
    ids: Float64Array;
    tagOffsets: Uint32Array;
    tags: Uint32Array;
  };
  within?: {
    offsets: Uint32Array;
    areas: Uint32Array;
    types: Uint8Array;
    ids: Float64Array;
    tagOffsets: Uint32Array;
    tags: Uint32Array;
  };
}

export interface NativeElementBatch {
  kind: "elements";
  strings: string[];
  types: Uint8Array;
  ids: Float64Array;
  tagOffsets: Uint32Array;
  tags: Uint32Array;
  locations: Float64Array;
  refOffsets: Uint32Array;
  refs: Float64Array;
  hasWayCoords: Uint8Array;
  wayCoordOffsets: Uint32Array;
  wayCoords: Float64Array;
  memberOffsets: Uint32Array;
  memberTypes: Uint8Array;
  memberIds: Float64Array;
  memberRoles: Uint32Array;
  meta?: NativeMeta;
}

export interface NativeTileBatch {
  kind: "tiles";
  z: Uint8Array;
  x: Uint32Array;
  y: Uint32Array;
  offsets: Uint32Array;
  data: Buffer;
}

export interface NativeTileOptions {
  configJson: string;
  processSource: string;
  processName?: string;
  bbox?: number[];
  threads?: number;
  compression?: string;
  batchSize?: number;
  readAhead?: number;
  output?: string;
}

export type NativeEvent =
  | { type: "batch"; batch: NativeFeatureBatch | NativeElementBatch | NativeTileBatch }
  | {
      type: "progress";
      phase: string;
      blocksDone?: number;
      blocksTotal?: number;
      zoom?: number;
      tilesDone?: number;
      tilesTotal?: number;
    }
  | { type: "drain" }
  | { type: "end"; stats: Record<string, number | boolean> }
  | { type: "error"; message: string };

export interface NativeStream {
  request(n: number): void;
  close(): void;
  setRef(referenced: boolean): void;
  release(): void;
  /** 0: keep pushing; 1: wait for "drain"; 2: the export is over. */
  push(chunk: Buffer): number;
  endInput(): void;
}

export interface NativeReader {
  readonly header: NativeHeader;
  readonly blockCount: number;
  readonly size: number;
  readonly path: string;
  features(options: NativeStreamOptions, callback: (event: NativeEvent) => void): NativeStream;
  elements(options: NativeStreamOptions, callback: (event: NativeEvent) => void): NativeStream;
  tiles(options: NativeTileOptions, callback: (event: NativeEvent) => void): NativeStream;
  writeTiles(options: NativeTileOptions, callback: (event: NativeEvent) => void): NativeStream;
  writeGeoJSONSeq(options: NativeStreamOptions, callback: (event: NativeEvent) => void): NativeStream;
  fileInfo(options: { extended?: boolean; threads?: number }, callback: (event: NativeEvent) => void): NativeStream;
  extract(options: NativeExtractOptions, callback: (event: NativeEvent) => void): NativeStream;
}

export interface NativeExtractOptions {
  output: string;
  filters?: string[];
  bbox?: number[];
  polygonJson?: string;
  polygonFile?: string;
  omitReferenced?: boolean;
  metadata?: boolean;
  threads?: number;
}

export class NativePolygonIndex {
  constructor();
  add(coords: Float64Array, parts: Uint32Array, polygons: Uint32Array): number;
  build(): void;
  readonly size: number;
  area(i: number): number;
  lookup(lon: number, lat: number): Uint32Array;
  smallest(lon: number, lat: number): number;
  smallestMany(points: Float64Array, threads?: number): Promise<Int32Array>;
}

export function open(path: string): Promise<NativeReader>;
export function elementsFromInput(options: NativeStreamOptions, callback: (event: NativeEvent) => void): NativeStream;
export function version(): string;
