import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import { beforeAll, describe, expect, it } from "vitest";

import { fixture } from "./helpers.js";

const root = resolve(__dirname, "..");
let work: string;

/** Run an example with `@tilemaker-rs/node` resolving to this package. */
function run(example: string, args: string[]) {
  const r = spawnSync(process.execPath, [join(work, "examples", example), ...args], { encoding: "utf8", cwd: work, timeout: 60_000 });
  if (r.status !== 0) throw new Error(`${example} failed: ${r.stderr}`);
  return r;
}

beforeAll(() => {
  work = mkdtempSync(join(tmpdir(), "tilemaker-node-examples-"));
  mkdirSync(join(work, "node_modules", "@tilemaker-rs"), { recursive: true });
  symlinkSync(root, join(work, "node_modules", "@tilemaker-rs", "node"), "junction");
  symlinkSync(join(root, "examples"), join(work, "examples"), "junction");
});

describe("examples", () => {
  it("geography", () => {
    const out = JSON.parse(run("geography.mjs", [fixture("town.osm.pbf")]).stdout);
    expect(out).toEqual({ boundaries: [], settlements: [] });
  });

  it("export-geojsonseq", () => {
    const out = join(work, "out.geojsonseq");
    run("export-geojsonseq.mjs", [fixture("town.osm.pbf"), out, "amenity", "w/highway"]);
    const lines = readFileSync(out, "utf8").trim().split("\n");
    expect(lines.map((l) => JSON.parse(l).id)).toEqual(["node/20", "way/100", "way/102"]);
  });

  it("pois-csv", () => {
    const out = run("pois-csv.mjs", [fixture("town.osm.pbf")]).stdout.trim().split("\n");
    expect(out).toEqual(["osm,kind,name,lon,lat", "node/20,amenity=cafe,Café,0.5000000,0.5000000", "node/21,shop=bakery,,10.0000000,10.0000000"]);
  });

  it("tiles", () => {
    writeFileSync(
      join(work, "config.json"),
      JSON.stringify({
        layers: { poi: { minzoom: 6, maxzoom: 10 } },
        settings: { minzoom: 6, maxzoom: 10, basezoom: 10, name: "x", version: "1", description: "", compress: "gzip" },
      }),
    );
    writeFileSync(join(work, "process.lua"), 'node_keys = { "amenity" }\nfunction node_function() Layer("poi") end');
    const r = run("tiles.mjs", [fixture("town.osm.pbf"), join(work, "t.pmtiles"), join(work, "config.json"), join(work, "process.lua")]);
    expect(r.stderr).toMatch(/5 tiles/);
    const s = run("tiles.mjs", [fixture("town.osm.pbf"), "--stream", join(work, "config.json"), join(work, "process.lua")]);
    expect(s.stderr).toMatch(/5 tiles, \d+ bytes gzipped/);
  });
});
