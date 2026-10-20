import { describe, expect, it } from "vitest";

import { openPbf, PolygonIndex, type Feature } from "../ts/index.js";
import { extract, fixture } from "./helpers.js";

const square = (id: number, x: number, y: number, s: number, tags: Record<string, string>): Feature => ({
  type: "relation",
  id,
  tags,
  geometry: {
    type: "MultiPolygon",
    coordinates: new Float64Array([x, y, x + s, y, x + s, y + s, x, y + s, x, y]),
    parts: new Uint32Array([5]),
    polygons: new Uint32Array([1]),
  },
});

describe("PolygonIndex", () => {
  it("finds the areas containing a point, smallest first", async () => {
    const index = await PolygonIndex.fromFeatures([
      square(1, 0, 0, 10, { admin_level: "2", name: "Land" }),
      square(2, 1, 1, 2, { admin_level: "4", name: "Province" }),
      square(3, 20, 20, 1, { admin_level: "2", name: "Island state" }),
      { type: "node", id: 9, tags: {}, geometry: { type: "Point", coordinates: new Float64Array([1, 1]) } },
    ]);
    expect(index.size).toBe(3);
    expect(index.lookup(1.5, 1.5).map((e) => e.tags.name)).toEqual(["Province", "Land"]);
    expect(index.find(1.5, 1.5)?.id).toBe(2);
    expect(index.find(1.5, 1.5, (e) => e.tags.admin_level === "2")?.id).toBe(1);
    expect(index.find(50, 50)).toBeUndefined();
    expect(index.entries[1]!.bbox).toEqual([1, 1, 3, 3]);
    expect(index.entries[0]!.area).toBeGreaterThan(index.entries[1]!.area);
    const at = await index.findMany([1.5, 1.5, 5, 5, 20.5, 20.5, 99, 99]);
    expect(Array.from(at)).toEqual([1, 0, 2, -1]);
    await expect(index.findMany([1])).rejects.toThrow(/pairs/);
  });

  it("indexes a file's areas", async () => {
    const index = await PolygonIndex.fromFile(fixture("town.osm.pbf"));
    // The building and the meadow (with its hole); the boundary relation is
    // incomplete in the file.
    expect(index.entries.map((e) => `${e.type}/${e.id}`)).toEqual(["way/101", "relation/200"]);
    expect(index.lookup(0.5, 0.5).map((e) => e.id)).toEqual([101]); // in the meadow's hole
    expect(index.lookup(0.2, 0.2).map((e) => e.id)).toEqual([200, 101]);
  });

  const li = extract("liechtenstein.osm.pbf");
  it.skipIf(!li)("reverse-geocodes places in a real extract", async () => {
    const regions = await PolygonIndex.fromFile(li!, { filters: ["r/admin_level=2,8"] });
    const pbf = await openPbf(li!);
    const places = await pbf.features({ filters: ["n/place=town,village"] }).toArray();
    expect(places.length).toBeGreaterThan(5);
    const at = await regions.findMany(Float64Array.from(places.flatMap((p) => Array.from(p.geometry.coordinates))));
    let inCountry = 0;
    for (let i = 0; i < places.length; i++) {
      const r = at[i]! >= 0 ? regions.entries[at[i]!]! : undefined;
      const country = regions.find(places[i]!.geometry.coordinates[0]!, places[i]!.geometry.coordinates[1]!, (e) => e.tags.admin_level === "2");
      if (country?.tags["ISO3166-1"] === "LI") inCountry++;
      // The smallest area is a municipality, inside the country.
      if (r) expect(["2", "8"]).toContain(r.tags.admin_level);
    }
    expect(inCountry).toBeGreaterThan(places.length / 2);
  });
});
