import { execFileSync } from "node:child_process";

import { describe, expect, it } from "vitest";

import { fileInfo, openPbf } from "../ts/index.js";
import { extract, fixture, hasOsmium } from "./helpers.js";

describe("fileInfo", () => {
  it("reads the header and blocks without decoding", async () => {
    const info = await fileInfo(fixture("town.osm.pbf"));
    expect(info.data).toBeNull();
    expect(info.blocks).toBeGreaterThan(0);
    expect(info.compression).toEqual({ zlib: info.blocks });
    expect(info.header.sorted).toBe(true);
    expect(info.header.writingProgram).toBe("make_test_fixtures");
    expect(info.size).toBeGreaterThan(0);
  });

  it("counts, ranges and dates every element with extended", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    const progress: number[] = [];
    const info = await pbf.fileInfo({ extended: true, onProgress: (p) => progress.push(p.blocksDone) });
    const d = info.data!;
    expect(d.counts).toEqual({ nodes: 15, ways: 6, relations: 3 });
    expect(d.tagged).toEqual({ nodes: 3, ways: 3, relations: 3 });
    expect(d.minId).toEqual({ nodes: 1, ways: 100, relations: 200 });
    expect(d.maxId).toEqual({ nodes: 14274176831, ways: 105, relations: 202 });
    expect(d.bbox).toEqual([-0.5, -0.5, 10, 10]);
    expect(d.wayNodes).toBe(2 + 5 + 5 + 3 + 3 + 5);
    expect(d.relationMembers).toBe(3 + 2 + 3);
    expect(d.timestamps).toEqual([1_600_000_000, 1_600_000_000]);
    expect(d.objectsOrdered).toBe(true);
    expect(d.multipleVersions).toBe(false);
    // Only two elements carry metadata.
    expect(d.metadata).toEqual({ all: [], some: ["version", "timestamp", "changeset", "uid", "user"] });
    expect(progress.at(-1)).toBe(info.blocks);
  });

  it("rejects unreadable files", async () => {
    await expect(fileInfo(fixture("not-a-pbf.osm.pbf"))).rejects.toThrow();
  });

  const monaco = extract("monaco.osm.pbf");
  it.skipIf(!monaco || !hasOsmium)("matches osmium fileinfo -e", async () => {
    const o = JSON.parse(execFileSync("osmium", ["fileinfo", "-e", "-j", monaco!], { encoding: "utf8" }));
    const d = (await fileInfo(monaco!, { extended: true })).data!;
    for (const t of ["nodes", "ways", "relations"] as const) {
      expect(d.counts[t]).toBe(o.data.count[t]);
      expect(d.minId[t]).toBe(o.data.minid[t]);
      expect(d.maxId[t]).toBe(o.data.maxid[t]);
    }
    expect(d.bbox).toEqual(o.data.bbox);
    expect(d.objectsOrdered).toBe(o.data.objects_ordered);
    expect(d.multipleVersions).toBe(o.data.multiple_versions);
    const iso = (s: number) => new Date(s * 1000).toISOString().replace(/\.000Z$/, "Z");
    expect(d.timestamps!.map(iso)).toEqual([o.data.timestamp.first, o.data.timestamp.last]);
  });
});
