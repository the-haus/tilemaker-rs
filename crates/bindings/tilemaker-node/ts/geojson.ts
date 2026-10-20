// GeoJSON conversion and geometry helpers.

import type { Feature, Geometry, MultiPolygonGeometry } from "./types.js";

export type Position = [number, number];

export interface GeoJSONGeometry {
  type: Geometry["type"];
  coordinates: Position | Position[] | Position[][] | Position[][][];
}

export interface GeoJSONFeature {
  type: "Feature";
  /** `"node/123"`, `"way/456"`, `"relation/789"`. */
  id: string;
  properties: Record<string, unknown>;
  /** The feature's `bbox`, when computed. */
  bbox?: [number, number, number, number];
  geometry: GeoJSONGeometry;
}

function positions(c: Float64Array, start: number, end: number): Position[] {
  const out = new Array<Position>(end - start);
  for (let i = start; i < end; i++) out[i - start] = [c[2 * i]!, c[2 * i + 1]!];
  return out;
}

/** A geometry as GeoJSON (nested coordinate arrays). */
export function geometryToGeoJSON(g: Geometry): GeoJSONGeometry {
  const c = g.coordinates;
  switch (g.type) {
    case "Point":
      return { type: "Point", coordinates: [c[0]!, c[1]!] };
    case "LineString":
      return { type: "LineString", coordinates: positions(c, 0, c.length / 2) };
    case "MultiLineString": {
      let start = 0;
      const lines = Array.from(g.parts, (end) => {
        const l = positions(c, start, end);
        start = end;
        return l;
      });
      return { type: "MultiLineString", coordinates: lines };
    }
    case "MultiPolygon":
      return { type: "MultiPolygon", coordinates: polygonRings(g).map((p) => p.map(([s, e]) => positions(c, s, e))) };
  }
}

/**
 * The rings of a multipolygon as `[start, end)` point ranges, per polygon:
 * `rings[p][0]` is polygon `p`'s exterior, the rest its holes.
 */
export function polygonRings(g: MultiPolygonGeometry): Array<Array<[number, number]>> {
  const out: Array<Array<[number, number]>> = [];
  let ring = 0;
  let point = 0;
  for (const ringEnd of g.polygons) {
    const rings: Array<[number, number]> = [];
    for (; ring < ringEnd; ring++) {
      const end = g.parts[ring]!;
      rings.push([point, end]);
      point = end;
    }
    out.push(rings);
  }
  return out;
}

/** Seconds since the epoch as ISO 8601, to the second (as osmium writes it). */
const iso = (seconds: number) => new Date(seconds * 1000).toISOString().replace(/\.\d{3}Z$/, "Z");

/**
 * A feature as GeoJSON: tags become properties, plus `@type` and `@id`
 * (like `osmium export -a type,id`), metadata as `@version`, … and computed
 * values as `@area`, `@length`, `@centroid`, `@labelPoint`, `@memberOf`
 * when present (the same as {@link writeGeoJSONSeq} writes).
 */
export function toGeoJSON(f: Feature): GeoJSONFeature {
  const properties: Record<string, unknown> = {};
  const set = (k: string, v: unknown) =>
    Object.defineProperty(properties, k, { value: v, enumerable: true, writable: true, configurable: true });
  for (const k of Object.keys(f.tags)) set(k, f.tags[k]);
  // The generated fields win over tags of the same name.
  set("@type", f.type);
  set("@id", f.id);
  if (f.meta) {
    set("@version", f.meta.version);
    set("@timestamp", iso(f.meta.timestamp));
    set("@changeset", f.meta.changeset);
    set("@uid", f.meta.uid);
    set("@user", f.meta.user);
  }
  if (f.area !== undefined) set("@area", f.area);
  if (f.length !== undefined) set("@length", f.length);
  if (f.centroid) set("@centroid", f.centroid);
  if (f.labelPoint) set("@labelPoint", f.labelPoint);
  if (f.memberOf) set("@memberOf", f.memberOf);
  if (f.within) set("@within", f.within);
  const geometry = geometryToGeoJSON(f.geometry);
  const id = `${f.type}/${f.id}`;
  return f.bbox
    ? { type: "Feature", id, properties, bbox: f.bbox, geometry }
    : { type: "Feature", id, properties, geometry };
}

/** `[west, south, east, north]` of a geometry. */
export function bbox(g: Geometry): [number, number, number, number] {
  const c = g.coordinates;
  let w = Infinity;
  let s = Infinity;
  let e = -Infinity;
  let n = -Infinity;
  for (let i = 0; i < c.length; i += 2) {
    const x = c[i]!;
    const y = c[i + 1]!;
    if (x < w) w = x;
    if (x > e) e = x;
    if (y < s) s = y;
    if (y > n) n = y;
  }
  return [w, s, e, n];
}

/**
 * Newline-delimited GeoJSON (one feature per line), e.g. to write a
 * `.geojsonseq` file: `pipeline(Readable.from(geoJSONSeq(stream)), createWriteStream(path))`.
 */
export async function* geoJSONSeq(features: AsyncIterable<Feature>): AsyncGenerator<string, void, undefined> {
  for await (const f of features) yield JSON.stringify(toGeoJSON(f)) + "\n";
}
