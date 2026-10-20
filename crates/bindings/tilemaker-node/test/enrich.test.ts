import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, describe, expect, it } from "vitest";

import { openPbf, toGeoJSON, writeGeoJSONSeq, type Feature, type FeatureOptions } from "../ts/index.js";
import { fixture } from "./helpers.js";

const town = fixture("town.osm.pbf");
const dir = mkdtempSync(join(tmpdir(), "tilemaker-enrich-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));

async function features(options: FeatureOptions = {}): Promise<Map<string, Feature>> {
  const pbf = await openPbf(town);
  const fs = await pbf.features(options).toArray();
  return new Map(fs.map((f) => [`${f.type}/${f.id}:${f.geometry.type}`, f]));
}

// One degree on the sphere tilemaker-rs uses (radius 6,371 km).
const DEG = (2 * Math.PI * 6_371_000) / 360;

describe("enrich", () => {
  it("computes area, length, centroid, label point and bbox", async () => {
    const fs = await features({ enrich: { area: true, length: true, centroid: true, labelPoint: true, bbox: true } });
    const building = fs.get("way/101:MultiPolygon")!;
    // A 1° square at the equator.
    expect(building.area! / (DEG * DEG)).toBeCloseTo(1, 2);
    expect(building.length! / (4 * DEG)).toBeCloseTo(1, 2);
    expect(building.centroid![0]).toBeCloseTo(0.5, 9);
    expect(building.centroid![1]).toBeCloseTo(0.5, 3);
    expect(building.bbox).toEqual([0, 0, 1, 1]);
    const [lx, ly] = building.labelPoint!;
    expect(lx > 0 && lx < 1 && ly > 0 && ly < 1).toBe(true);

    // The meadow has a hole in the middle: less area, and a label point
    // outside the hole.
    const meadow = fs.get("relation/200:MultiPolygon")!;
    expect(meadow.area! / (DEG * DEG)).toBeCloseTo(1 - 0.04, 2);
    const [mx, my] = meadow.labelPoint!;
    expect(mx > 0.4 && mx < 0.6 && my > 0.4 && my < 0.6).toBe(false);

    const street = fs.get("way/100:LineString")!;
    expect(street.length! / DEG).toBeCloseTo(1, 3);
    expect(street.area).toBe(0);
    expect(street.labelPoint).toEqual([0.5, 0]);

    const cafe = fs.get("node/20:Point")!;
    expect(cafe).toMatchObject({ area: 0, length: 0, centroid: [0.5, 0.5], labelPoint: [0.5, 0.5], bbox: [0.5, 0.5, 0.5, 0.5] });
  });

  it("leaves features alone without enrich", async () => {
    const fs = await features();
    const building = fs.get("way/101:MultiPolygon")!;
    expect(Object.keys(building).sort()).toEqual(["geometry", "id", "tags", "type"]);
  });

  it("lists parent relations with memberOf", async () => {
    const all = await features({ enrich: { memberOf: true } });
    expect(all.get("way/100:LineString")!.memberOf).toEqual([{ id: 202, role: "", tags: { type: "route", route: "bus" } }]);
    expect(all.get("node/20:Point")!.memberOf).toEqual([{ id: 202, role: "stop", tags: { type: "route", route: "bus" } }]);
    expect(all.get("way/101:MultiPolygon")!.memberOf).toEqual([
      { id: 201, role: "outer", tags: { type: "boundary", boundary: "administrative", admin_level: "8" } },
    ]);
    expect(all.get("node/21:Point")!.memberOf).toEqual([]);
    // Parents' tags are shared between their members' features.
    expect(all.get("way/100:LineString")!.memberOf![0]!.tags).toBe(all.get("node/20:Point")!.memberOf![0]!.tags);

    const routes = await features({ enrich: { memberOf: ["route=bus"] } });
    expect(routes.get("way/101:MultiPolygon")!.memberOf).toEqual([]);
    expect(routes.get("way/100:LineString")!.memberOf!.map((p) => p.id)).toEqual([202]);
  });

  it("places features in the areas they lie in with within", async () => {
    const fs = await features({ enrich: { within: ["a/landuse", "a/building"] } });
    // The café is in the meadow's hole but on the building.
    expect(fs.get("node/20:Point")!.within!.map((a) => `${a.type}/${a.id}`)).toEqual(["way/101"]);
    // The tree is outside both; the street's midpoint (0.5, 0) is on both edges.
    expect(fs.get("node/14274176831:Point")!.within).toEqual([]);
    const street = fs.get("way/100:LineString")!.within!;
    expect(street.map((a) => a.id).sort()).toEqual([101, 200]);
    expect(street.find((a) => a.id === 200)!.tags).toEqual({ type: "multipolygon", landuse: "meadow" });
    // In GeoJSON: @within.
    expect(toGeoJSON(fs.get("node/20:Point")!).properties["@within"]).toEqual([
      { type: "way", id: 101, tags: { building: "yes" } },
    ]);
  });

  it("clips, simplifies and rounds geometries", async () => {
    const clipped = await features({ clip: [0, 0, 0.5, 0.5], enrich: { bbox: true } });
    expect(clipped.get("way/101:MultiPolygon")!.bbox).toEqual([0, 0, 0.5, 0.5]);
    expect(clipped.has("node/20:Point")).toBe(true); // on the edge
    expect(clipped.has("node/21:Point")).toBe(false);
    expect(clipped.has("way/102:LineString")).toBe(false);
    // `bbox` selects whole features; `clip` cuts them.
    const selected = await features({ bbox: [0, 0, 0.5, 0.5], enrich: { bbox: true } });
    expect(selected.get("way/101:MultiPolygon")!.bbox).toEqual([0, 0, 1, 1]);

    const rounded = await features({ precision: 1 });
    for (const f of rounded.values()) {
      expect(Array.from(f.geometry.coordinates).every((v) => Math.abs(v * 10 - Math.round(v * 10)) < 1e-9)).toBe(true);
    }
    // At 0 decimals the 0.2° hole of the meadow collapses onto its exterior's corners.
    const meadow = (await features({ precision: 0 })).get("relation/200:MultiPolygon")!;
    expect(Array.from(meadow.geometry.coordinates).every(Number.isInteger)).toBe(true);

    const simplified = await features({ simplify: 10 });
    // A tolerance larger than the town collapses its polygons; lines keep their ends.
    expect(simplified.has("way/101:MultiPolygon")).toBe(false);
    expect(Array.from(simplified.get("way/100:LineString")!.geometry.coordinates)).toEqual([0, 0, 1, 0]);
  });

  it("validates the new options", async () => {
    const pbf = await openPbf(town);
    await expect(pbf.features({ clip: [1, 1, 0, 0] }).toArray()).rejects.toThrow(/clip/);
    await expect(pbf.features({ simplify: -1 }).toArray()).rejects.toThrow(/simplify/);
    await expect(pbf.features({ enrich: { memberOf: ["=x"] } }).toArray()).rejects.toThrow(/filter/);
  });
});

describe("writeGeoJSONSeq", () => {
  it("writes what toGeoJSON makes, without features crossing into JS", async () => {
    const options: FeatureOptions = {
      metadata: true,
      enrich: {
        area: true,
        length: true,
        centroid: true,
        labelPoint: true,
        bbox: true,
        memberOf: true,
        within: ["a/landuse"],
      },
    };
    const output = join(dir, "town.geojsonseq");
    const stats = await writeGeoJSONSeq(town, { ...options, output });
    const lines = readFileSync(output, "utf8").trimEnd().split("\n");
    expect(stats.emitted).toBe(lines.length);
    const written = lines.map((l) => JSON.parse(l));
    const pbf = await openPbf(town);
    const expected = (await pbf.features(options).toArray()).map((f) => JSON.parse(JSON.stringify(toGeoJSON(f))));
    expect(written.length).toBe(expected.length);
    for (let i = 0; i < written.length; i++) {
      const w = written[i];
      const e = expected[i];
      // Measures are written to the millimetre.
      for (const k of ["@area", "@length"]) {
        expect(w.properties[k]).toBeCloseTo(e.properties[k], 2);
        delete w.properties[k];
        delete e.properties[k];
      }
      expect(w).toEqual(e);
    }
    expect(written.find((f) => f.id === "node/20").properties["@timestamp"]).toBe("2020-09-13T12:26:40Z");
  });

  it("filters, writes record separators, and reports errors", async () => {
    const output = join(dir, "cafes.geojsonseq");
    const pbf = await openPbf(town);
    const stats = await pbf.writeGeoJSONSeq({ filters: ["n/amenity"], output, recordSeparator: true });
    expect(stats.emitted).toBe(1);
    const text = readFileSync(output, "utf8");
    expect(text.startsWith("\x1e{")).toBe(true);
    expect(JSON.parse(text.slice(1)).properties.name).toBe("Café");
    await expect(writeGeoJSONSeq(town, { output: join(dir, "missing", "x.geojsonseq") })).rejects.toThrow(
      /missing/,
    );
  });
});
