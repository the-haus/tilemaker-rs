// Point-in-polygon lookups over many areas: which contain a point,
// smallest first. The geometry stays native; the entries (type, id, tags,
// area, bbox) stay here.

import { bbox } from "./geojson.js";
// A cycle (index.ts re-exports this module), resolved by the time it is called.
import { streamFeatures } from "./index.js";
import * as native from "./native.js";
import type { Feature, FeatureOptions, OsmType, Tags } from "./types.js";

/** An area in a {@link PolygonIndex}. */
export interface PolygonEntry {
  /** The element the area was made from. */
  type: OsmType;
  id: number;
  tags: Tags;
  /** Area in m² (on the sphere). */
  area: number;
  /** `[west, south, east, north]`. */
  bbox: [number, number, number, number];
}

/**
 * Areas indexed for point lookups — reverse geocoding a place to the
 * country and region it lies in, for instance. Build one from features
 * ({@link PolygonIndex.fromFeatures}) or straight from a file
 * ({@link PolygonIndex.fromFile}); only polygons are indexed.
 *
 * Polygon parts sit in an R-tree, and every ring has a banded edge index,
 * so a lookup against a million-point coastline checks about a thousand
 * edges. A point on a boundary counts as inside; holes are respected.
 *
 * ```ts
 * const countries = await PolygonIndex.fromFile("planet.osm.pbf", { filters: ["r/admin_level=2"] });
 * countries.find(4.9, 52.37)?.tags.name; // "Nederland"
 * const at = await countries.findMany(lonLats); // Int32Array of entry indices, -1 where none
 * ```
 */
export class PolygonIndex {
  /** The indexed areas, in the order added (lookups return these). */
  readonly entries: readonly PolygonEntry[];
  readonly #native: native.NativePolygonIndex;

  private constructor(n: native.NativePolygonIndex, entries: PolygonEntry[]) {
    this.#native = n;
    this.entries = entries;
  }

  /** Index the polygon features of a stream (or any iterable of features); others are skipped. */
  static async fromFeatures(features: AsyncIterable<Feature> | Iterable<Feature>): Promise<PolygonIndex> {
    const n = new native.NativePolygonIndex();
    const entries: PolygonEntry[] = [];
    for await (const f of features) {
      const g = f.geometry;
      if (g.type !== "MultiPolygon") continue;
      const i = n.add(g.coordinates, g.parts, g.polygons);
      entries[i] = { type: f.type, id: f.id, tags: f.tags, area: 0, bbox: bbox(g) };
    }
    n.build();
    for (let i = 0; i < entries.length; i++) entries[i]!.area = n.area(i);
    return new PolygonIndex(n, entries);
  }

  /**
   * Index the areas of a PBF file: {@link streamFeatures} with these
   * options, polygons only (closed ways and multipolygon/boundary
   * relations).
   */
  static async fromFile(path: string | URL, options: FeatureOptions = {}): Promise<PolygonIndex> {
    return PolygonIndex.fromFeatures(streamFeatures(path, { ...options, geometryTypes: ["polygon"] }));
  }

  /** Number of areas. */
  get size(): number {
    return this.entries.length;
  }

  /** The areas containing `(lon, lat)`, smallest first. */
  lookup(lon: number, lat: number): PolygonEntry[] {
    return Array.from(this.#native.lookup(lon, lat), (i) => this.entries[i]!);
  }

  /**
   * The smallest area containing `(lon, lat)` — of those `predicate`
   * accepts, when given (e.g. `(e) => e.tags.admin_level === "4"`).
   */
  find(lon: number, lat: number, predicate?: (entry: PolygonEntry) => boolean): PolygonEntry | undefined {
    if (!predicate) {
      const i = this.#native.smallest(lon, lat);
      return i < 0 ? undefined : this.entries[i];
    }
    return this.lookup(lon, lat).find(predicate);
  }

  /**
   * For many points (`[lon, lat, lon, lat, …]`), the index into
   * {@link entries} of the smallest area containing each, or -1 — on
   * worker threads, off the event loop.
   */
  findMany(points: Float64Array | ArrayLike<number>, options: { threads?: number } = {}): Promise<Int32Array> {
    const p = points instanceof Float64Array ? points : Float64Array.from(points);
    if (p.length % 2 !== 0) return Promise.reject(new RangeError("points must be lon, lat pairs"));
    return this.#native.smallestMany(p, options.threads);
  }
}
