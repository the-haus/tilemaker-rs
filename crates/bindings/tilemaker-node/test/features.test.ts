import { describe, expect, it } from "vitest";

import {
  bbox,
  geometryToGeoJSON,
  openPbf,
  polygonRings,
  streamFeatures,
  toGeoJSON,
  type Feature,
  type FeatureOptions,
  type MultiPolygonGeometry,
} from "../ts/index.js";
import { fixture, signedArea } from "./helpers.js";

async function collect(options: FeatureOptions = {}, file = "town.osm.pbf"): Promise<Feature[]> {
  const pbf = await openPbf(fixture(file));
  return pbf.features(options).toArray();
}

const summary = (fs: Feature[]) => fs.map((f) => `${f.type}/${f.id}:${f.geometry.type}`);

describe("features", () => {
  it("assembles everything by default", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    const stream = pbf.features();
    const fs = await stream.toArray();
    expect(summary(fs)).toEqual([
      "node/20:Point",
      "node/21:Point",
      "node/14274176831:Point",
      "way/100:LineString",
      "way/101:MultiPolygon",
      "way/102:LineString",
      "relation/200:MultiPolygon",
    ]);
    const stats = await stream.stats;
    expect(stats).toMatchObject({
      emitted: 7,
      points: 3,
      lines: 2,
      polygons: 2,
      multiLines: 0,
      relationsMissingWays: 1,
      waysMissingNodes: 0,
      invalidAreas: 0,
      stopped: false,
    });
    expect(stats.storedNodes).toBeGreaterThan(0);
  });

  it("gives exact coordinates and tags", async () => {
    const fs = await collect();
    const cafe = fs[0]!;
    expect(cafe.tags).toEqual({ amenity: "cafe", name: "Café" });
    expect(Array.from(cafe.geometry.coordinates)).toEqual([0.5, 0.5]);
    expect(cafe.meta).toBeUndefined();
    const street = fs.find((f) => f.id === 100)!;
    expect(Array.from(street.geometry.coordinates)).toEqual([0, 0, 1, 0]);
    // A `__proto__` key is an ordinary own property.
    const bakery = fs[1]!;
    expect(Object.keys(bakery.tags)).toEqual(["shop", "__proto__"]);
    expect(bakery.tags["__proto__"]).toBe("x");
    expect(Object.getPrototypeOf(bakery.tags)).toBe(Object.prototype);
  });

  it("assembles multipolygons with holes and RFC 7946 winding", async () => {
    const fs = await collect({ filters: ["r/landuse"] });
    expect(fs).toHaveLength(1);
    const g = fs[0]!.geometry as MultiPolygonGeometry;
    expect(g.type).toBe("MultiPolygon");
    expect(Array.from(g.polygons)).toEqual([2]);
    expect(Array.from(g.parts)).toEqual([5, 10]);
    expect(polygonRings(g)).toEqual([
      [
        [0, 5],
        [5, 10],
      ],
    ]);
    expect(signedArea(g.coordinates, 0, 5)).toBeCloseTo(1, 12);
    expect(signedArea(g.coordinates, 5, 10)).toBeCloseTo(-0.04, 12);
    expect(bbox(g)).toEqual([0, 0, 1, 1]);
  });

  it("converts to GeoJSON", async () => {
    const fs = await collect({ filters: ["r/landuse", "w/highway=residential"] });
    const line = toGeoJSON(fs[0]!);
    expect(line).toEqual({
      type: "Feature",
      id: "way/100",
      properties: { "@type": "way", "@id": 100, highway: "residential", name: "Main St" },
      geometry: {
        type: "LineString",
        coordinates: [
          [0, 0],
          [1, 0],
        ],
      },
    });
    const area = toGeoJSON(fs[1]!);
    expect(area.id).toBe("relation/200");
    const coords = area.geometry.coordinates as number[][][][];
    expect(coords).toHaveLength(1);
    expect(coords[0]).toHaveLength(2);
    expect(coords[0]![0]![0]).toEqual(coords[0]![0]![4]);
    expect(JSON.parse(JSON.stringify(area))).toEqual(area);
  });

  it("filters with osmium syntax", async () => {
    expect(summary(await collect({ filters: ["n/amenity=cafe,pub"] }))).toEqual(["node/20:Point"]);
    expect(summary(await collect({ filters: ["highway!=residential"] }))).toEqual(["way/102:LineString"]);
    expect(summary(await collect({ filters: ["a/building"] }))).toEqual(["way/101:MultiPolygon"]);
    expect(summary(await collect({ filters: ["name=*St"] }))).toEqual(["way/100:LineString"]);
    expect(summary(await collect({ filters: ["n/natural", "w/junction"] }))).toEqual([
      "node/14274176831:Point",
      "way/102:LineString",
    ]);
  });

  it("selects geometry types", async () => {
    expect(summary(await collect({ geometryTypes: ["polygon"] }))).toEqual([
      "way/101:MultiPolygon",
      "relation/200:MultiPolygon",
    ]);
    expect(summary(await collect({ geometryTypes: ["linestring"] }))).toEqual([
      "way/100:LineString",
      "way/102:LineString",
    ]);
    const stream = (await openPbf(fixture("town.osm.pbf"))).features({ geometryTypes: ["point"] });
    expect(await stream.toArray()).toHaveLength(3);
    expect((await stream.stats).storedNodes).toBe(0);
  });

  it("decides closed ways", async () => {
    const run = (closedWays: FeatureOptions["closedWays"]) =>
      collect({ filters: ["w/building", "w/junction"], closedWays }).then(summary);
    expect(await run("auto")).toEqual(["way/101:MultiPolygon", "way/102:LineString"]);
    expect(await run("polygon")).toEqual(["way/101:MultiPolygon", "way/102:MultiPolygon"]);
    expect(await run("linestring")).toEqual(["way/101:LineString", "way/102:LineString"]);
    expect(await run("both")).toEqual([
      "way/101:MultiPolygon",
      "way/101:LineString",
      "way/102:MultiPolygon",
      "way/102:LineString",
    ]);
  });

  it("builds relation lines", async () => {
    const fs = await collect({ filters: ["r/route"], relationLines: true });
    expect(summary(fs)).toEqual(["relation/202:MultiLineString"]);
    const g = fs[0]!.geometry;
    expect(g.type === "MultiLineString" && Array.from(g.parts)).toEqual([2, 7]);
    expect(geometryToGeoJSON(g).coordinates).toHaveLength(2);
  });

  it("selects tags, metadata and a bbox", async () => {
    const fs = await collect({
      filters: ["n/amenity", "n/shop", "w/highway=residential"],
      tags: ["name"],
      metadata: true,
      bbox: [-1, -1, 5, 5],
    });
    expect(summary(fs)).toEqual(["node/20:Point", "way/100:LineString"]);
    expect(fs[0]!.tags).toEqual({ name: "Café" });
    expect(fs[0]!.meta).toEqual({ version: 2, timestamp: 1_600_000_000, changeset: 9, uid: 3, user: "alice", visible: true });
    expect(fs[1]!.meta?.user).toBe("bob");
    const noTags = await collect({ filters: ["n/amenity"], tags: false });
    expect(noTags[0]!.tags).toEqual({});
    const wild = await collect({ filters: ["n/amenity"], tags: ["n*"] });
    expect(wild[0]!.tags).toEqual({ name: "Café" });
    expect(toGeoJSON(fs[0]!).properties["@timestamp"]).toBe("2020-09-13T12:26:40.000Z");
  });

  it("reads LocationsOnWays files without a node store", async () => {
    const stream = (await openPbf(fixture("low.osm.pbf"))).features();
    const fs = await stream.toArray();
    expect(Array.from(fs[0]!.geometry.coordinates)).toEqual([2, 1, 4, 3, 6, 5]);
    expect((await stream.stats).storedNodes).toBe(0);
  });

  it("supports a dense on-disk node store", async () => {
    const { tmpdir } = await import("node:os");
    const { existsSync } = await import("node:fs");
    const { join } = await import("node:path");
    const file = join(tmpdir(), `tilemaker-node-store-${process.pid}.bin`);
    const a = await collect({ nodeStoreFile: file });
    const b = await collect();
    expect(summary(a)).toEqual(summary(b));
    expect(a.map((f) => Array.from(f.geometry.coordinates))).toEqual(b.map((f) => Array.from(f.geometry.coordinates)));
    expect(existsSync(file)).toBe(false);
  });

  it("skips ways with missing nodes and counts them", async () => {
    const stream = (await openPbf(fixture("broken.osm.pbf"))).features();
    const fs = await stream.toArray();
    expect(fs.map((f) => f.id)).toEqual([11]);
    expect((await stream.stats).waysMissingNodes).toBe(1);
  });

  it("streamFeatures opens lazily", async () => {
    const fs = await streamFeatures(fixture("town.osm.pbf"), { filters: ["n/amenity"] }).toArray();
    expect(fs).toHaveLength(1);
    await expect(streamFeatures(fixture("missing.osm.pbf")).toArray()).rejects.toThrow(/missing\.osm\.pbf/);
  });

  it("rejects invalid options when iteration starts", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    await expect(pbf.features({ filters: ["w/"] }).toArray()).rejects.toThrow(/invalid tag filter/);
    await expect(pbf.features({ geometryTypes: ["blob" as "point"] }).toArray()).rejects.toThrow(/unknown geometry type/);
    await expect(pbf.features({ closedWays: "maybe" as "auto" }).toArray()).rejects.toThrow(/closedWays/);
    await expect(pbf.features({ bbox: [5, 5, 1, 1] }).toArray()).rejects.toThrow(/bbox/);
  });
});
