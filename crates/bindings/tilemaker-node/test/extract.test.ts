import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, describe, expect, it } from "vitest";

import { extract, openPbf, streamElements, type ExtractOptions } from "../ts/index.js";
import { extract as fixtureExtract, fixture, hasOsmium } from "./helpers.js";

const dir = mkdtempSync(join(tmpdir(), "tilemaker-extract-"));
afterAll(() => rmSync(dir, { recursive: true, force: true }));

let n = 0;
async function ids(input: string, options: Omit<ExtractOptions, "output">): Promise<string[]> {
  const output = join(dir, `out-${n++}.osm.pbf`);
  await extract(input, { ...options, output });
  const out: string[] = [];
  for await (const e of streamElements(output, { includeUntagged: true })) out.push(`${e.type}/${e.id}`);
  return out;
}

const town = fixture("town.osm.pbf");

describe("extract", () => {
  it("keeps matching elements and what they reference", async () => {
    // The bus route: its ways, their nodes, and its stop.
    expect(await ids(town, { filters: ["r/route=bus"] })).toEqual([
      "node/1",
      "node/2",
      "node/9",
      "node/10",
      "node/11",
      "node/12",
      "node/20",
      "way/100",
      "way/102",
      "relation/202",
    ]);
    expect(await ids(town, { filters: ["r/route=bus"], omitReferenced: true })).toEqual(["relation/202"]);
  });

  it("cuts regions by box and polygon", async () => {
    const box = await ids(town, { bbox: [-0.1, -0.1, 0.45, 0.45] });
    // Nodes inside, ways touching (completed), relations with them and parents.
    expect(box).toContain("node/1");
    expect(box).toContain("way/101");
    expect(box).toContain("node/3"); // completes way 101
    expect(box).not.toContain("node/21");
    const polygon = {
      type: "Polygon",
      coordinates: [
        [
          [-0.1, -0.1],
          [0.45, -0.1],
          [0.45, 0.45],
          [-0.1, 0.45],
          [-0.1, -0.1],
        ],
      ],
    };
    expect(await ids(town, { polygon })).toEqual(box);
    const file = join(dir, "region.geojson");
    writeFileSync(file, JSON.stringify({ type: "Feature", properties: {}, geometry: polygon }));
    expect(await ids(town, { polygon: file })).toEqual(box);
  });

  it("reports counters and progress, and validates options", async () => {
    const pbf = await openPbf(town);
    const phases = new Set<string>();
    const stats = await pbf.extract({
      output: join(dir, "all.osm.pbf"),
      onProgress: (p) => phases.add(p.phase),
    });
    expect(stats).toMatchObject({ nodes: 15, ways: 6, relations: 3, stopped: false });
    expect(stats.bytes).toBeGreaterThan(0);
    expect(phases.has("write")).toBe(true);
    await expect(pbf.extract({ output: join(dir, "x.osm.pbf"), bbox: [1, 1, 0, 0] })).rejects.toThrow(/bbox/);
    await expect(pbf.extract({ output: join(dir, "x.osm.pbf"), polygon: { type: "Point" } })).rejects.toThrow(
      /polygon/,
    );
    await expect(pbf.extract({ output: join(dir, "x.osm.pbf"), filters: ["="] })).rejects.toThrow();
  });

  const monaco = fixtureExtract("monaco.osm.pbf");
  const opl = (path: string) => execFileSync("osmium", ["cat", path, "-f", "opl"], { encoding: "utf8", maxBuffer: 1 << 30 });
  it.skipIf(!monaco || !hasOsmium)("writes what osmium writes", async () => {
    const cases: Array<[string[], Omit<ExtractOptions, "output">]> = [
      [["tags-filter", monaco!, "w/highway", "r/type=route"], { filters: ["w/highway", "r/type=route"] }],
      [["tags-filter", "-R", monaco!, "n/amenity"], { filters: ["n/amenity"], omitReferenced: true }],
      [["extract", "-b", "7.41,43.725,7.42,43.735", monaco!], { bbox: [7.41, 43.725, 7.42, 43.735] }],
    ];
    for (const [args, options] of cases) {
      const theirs = join(dir, `osmium-${n}.osm.pbf`);
      const ours = join(dir, `ours-${n++}.osm.pbf`);
      execFileSync("osmium", [...args, "-o", theirs, "-O"], { stdio: "ignore" });
      await extract(monaco!, { ...options, output: ours });
      expect(opl(ours)).toBe(opl(theirs));
    }
  });
});
