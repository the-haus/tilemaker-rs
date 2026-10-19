import { describe, expect, it } from "vitest";

import { openPbf, streamElements, type Element, type ElementOptions } from "../ts/index.js";
import { fixture } from "./helpers.js";

async function collect(options: ElementOptions = {}, file = "town.osm.pbf"): Promise<Element[]> {
  return (await openPbf(fixture(file))).elements(options).toArray();
}

describe("elements", () => {
  it("streams tagged elements in one pass", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    const progress: string[] = [];
    const stream = pbf.elements({ onProgress: (p) => progress.push(p.phase) });
    const es = await stream.toArray();
    expect(es.map((e) => `${e.type}/${e.id}`)).toEqual([
      "node/20",
      "node/21",
      "node/14274176831",
      "way/100",
      "way/101",
      "way/102",
      "relation/200",
      "relation/201",
      "relation/202",
    ]);
    expect(new Set(progress)).toEqual(new Set(["elements"]));
    expect((await stream.stats).storedNodes).toBe(0);
  });

  it("gives nodes, way refs and relation members", async () => {
    const es = await collect({ filters: ["n/amenity", "w/highway=primary", "r/route"] });
    const [node, way, rel] = es;
    expect(node).toEqual({ type: "node", id: 20, tags: { amenity: "cafe", name: "Café" }, lon: 0.5, lat: 0.5 });
    expect(way?.type).toBe("way");
    if (way?.type !== "way") throw new Error("expected a way");
    expect(Array.from(way.refs)).toEqual([9, 10, 11, 12, 9]);
    expect(way.coordinates).toBeUndefined();
    expect(rel).toEqual({
      type: "relation",
      id: 202,
      tags: { type: "route", route: "bus" },
      members: [
        { type: "way", ref: 100, role: "" },
        { type: "way", ref: 102, role: "" },
        { type: "node", ref: 20, role: "stop" },
      ],
    });
  });

  it("adds way coordinates on request", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    const stream = pbf.elements({ filters: ["w/highway"], wayLocations: true });
    const es = await stream.toArray();
    expect(es).toHaveLength(2);
    const w = es[1]!;
    if (w.type !== "way") throw new Error("expected a way");
    expect(Array.from(w.coordinates!)).toEqual([2, 0, 3, 0, 3, 1, 2, 1, 2, 0]);
    expect((await stream.stats).storedNodes).toBe(6);
  });

  it("marks unknown way nodes as NaN", async () => {
    const es = await collect({ wayLocations: true }, "broken.osm.pbf");
    const coords = es.map((e) => (e.type === "way" ? Array.from(e.coordinates!) : []));
    expect(coords[0]).toEqual([1, 1, NaN, NaN, 2, 2]);
    expect(coords[1]).toEqual([1, 1, 2, 2]);
  });

  it("selects types and untagged elements", async () => {
    const nodes = await collect({ types: ["node"], includeUntagged: true });
    expect(nodes).toHaveLength(15);
    expect(nodes.every((e) => e.type === "node")).toBe(true);
    const rels = await collect({ types: ["relation"] });
    expect(rels.map((e) => e.id)).toEqual([200, 201, 202]);
    await expect(collect({ types: ["area" as "node"] })).rejects.toThrow(/unknown element type/);
  });

  it("filters nodes and located ways by bbox", async () => {
    const es = await collect({ filters: ["n/shop", "n/amenity", "w/highway"], wayLocations: true, bbox: [1.5, -1, 5, 5] });
    expect(es.map((e) => `${e.type}/${e.id}`)).toEqual(["way/102"]);
  });

  it("includes metadata", async () => {
    const es = await collect({ filters: ["n/amenity", "w/highway=residential"], metadata: true });
    expect(es[0]!.meta).toEqual({ version: 2, timestamp: 1_600_000_000, changeset: 9, uid: 3, user: "alice", visible: true });
    expect(es[1]!.meta?.user).toBe("bob");
  });

  it("works on a path", async () => {
    const es = await streamElements(fixture("town.osm.pbf"), { filters: ["r/type=route"] }).toArray();
    expect(es.map((e) => e.id)).toEqual([202]);
  });
});
