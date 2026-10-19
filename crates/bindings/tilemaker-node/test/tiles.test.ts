import { existsSync, mkdtempSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gunzipSync } from "node:zlib";

import { VectorTile } from "@mapbox/vector-tile";
import { PbfReader as Pbf } from "pbf";
import { describe, expect, it } from "vitest";

import { generateTiles, openPbf, writeTiles, type Tile, type TileProgress } from "../ts/index.js";
import { extract, fixture } from "./helpers.js";

const resources = join(__dirname, "..", "..", "..", "..", "resources");

const config = {
  layers: {
    poi: { minzoom: 10, maxzoom: 12 },
    roads: { minzoom: 4, maxzoom: 12 },
    areas: { minzoom: 6, maxzoom: 12 },
  },
  settings: {
    minzoom: 4,
    maxzoom: 12,
    basezoom: 12,
    include_ids: true,
    name: "Town",
    version: "1",
    description: "test",
    compress: "gzip",
  },
};

const profile = {
  name: "town.lua",
  source: `
    node_keys = { "amenity", "shop" }
    function node_function()
      Layer("poi")
      Attribute("name", Find("name"))
      Attribute("class", Find("amenity") ~= "" and Find("amenity") or Find("shop"))
    end
    function way_function()
      if Find("highway") ~= "" then
        Layer("roads", false)
        Attribute("class", Find("highway"))
        AttributeBoolean("roundabout", Find("junction") == "roundabout")
      elseif Find("building") ~= "" or Find("landuse") ~= "" then
        Layer("areas", true)
        Attribute("kind", Find("building") ~= "" and "building" or Find("landuse"))
        AttributeNumeric("area", Area())
      end
    end
  `,
};

function decode(t: Tile): VectorTile {
  return new VectorTile(new Pbf(t.data));
}

describe("tiles", () => {
  it("streams tiles zoom by zoom", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    const progress: TileProgress[] = [];
    const stream = pbf.tiles({ config, process: profile, onProgress: (p) => progress.push(p) });
    const tiles = await stream.toArray();
    const zooms = tiles.map((t) => t.z);
    expect(zooms).toEqual([...zooms].sort((a, b) => a - b));
    expect(new Set(zooms)).toEqual(new Set([4, 5, 6, 7, 8, 9, 10, 11, 12]));
    const stats = await stream.stats;
    expect(stats.tiles).toBe(tiles.length);
    expect(stats.tilesPerZoom.reduce((a, b) => a + b, 0)).toBe(tiles.length);
    // The café and the bakery, two roads, the building and the meadow multipolygon.
    expect(stats.read.objects).toBe(6);
    expect(stats.metadata.name).toBe("Town");
    expect(stats.metadata.vector_layers.map((l) => l.id)).toEqual(["poi", "roads", "areas"]);
    expect(stats.metadata.vector_layers[1]!.fields).toEqual({ class: "String", roundabout: "Boolean" });
    expect(stats.metadata.bounds).toEqual([-1, -1, 11, 11]);
    expect(progress.some((p) => p.phase === "nodes")).toBe(true);
    expect(progress.some((p) => p.phase === "tiles" && p.zoom === 12)).toBe(true);
  });

  it("writes correct features", async () => {
    const tiles = await generateTiles(fixture("town.osm.pbf"), { config, process: profile }).toArray();
    // The café at (0.5, 0.5).
    const z12 = tiles.filter((t) => t.z === 12);
    const cafeTile = z12.find((t) => decode(t).layers["poi"]?.feature(0).properties["name"] === "Café");
    expect(cafeTile).toBeDefined();
    const poi = decode(cafeTile!).layers["poi"]!;
    const cafe = poi.feature(0);
    expect(cafe.id).toBe(20);
    expect(cafe.properties).toEqual({ name: "Café", class: "cafe" });
    const geo = cafe.toGeoJSON(cafeTile!.x, cafeTile!.y, cafeTile!.z);
    expect(geo.geometry.type).toBe("Point");
    const [lon, lat] = (geo.geometry as { coordinates: number[] }).coordinates;
    expect(lon).toBeCloseTo(0.5, 3);
    expect(lat).toBeCloseTo(0.5, 3);

    // The roundabout (a closed way, kept as a line) and the building.
    let roundabout = false;
    let building = false;
    for (const t of z12) {
      const vt = decode(t);
      const roads = vt.layers["roads"];
      for (let i = 0; roads && i < roads.length; i++) {
        if (roads.feature(i).properties["roundabout"] === true) roundabout = true;
      }
      const areas = vt.layers["areas"];
      for (let i = 0; areas && i < areas.length; i++) {
        const f = areas.feature(i);
        if (f.properties["kind"] === "building") {
          building = true;
          expect(f.type).toBe(3);
          expect(f.properties["area"]).toBeGreaterThan(1e10);
        }
      }
    }
    expect(roundabout).toBe(true);
    expect(building).toBe(true);
  });

  it("compresses on request", async () => {
    const tiles = await generateTiles(fixture("town.osm.pbf"), { config, process: profile, compression: "gzip" }).toArray();
    expect(tiles[0]!.data[0]).toBe(0x1f);
    const raw = gunzipSync(tiles[0]!.data);
    expect(Object.keys(new VectorTile(new Pbf(raw)).layers).length).toBeGreaterThan(0);
  });

  it("stops when the loop breaks", async () => {
    const stream = generateTiles(fixture("town.osm.pbf"), { config, process: profile, batchSize: 1 });
    let n = 0;
    for await (const t of stream) {
      void t;
      if (++n === 2) break;
    }
    expect((await stream.stats).stopped).toBe(true);
  });

  it("writes mbtiles, pmtiles and directories", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tilemaker-node-tiles-"));
    for (const out of ["town.mbtiles", "town.pmtiles", "town"]) {
      const stats = await writeTiles(fixture("town.osm.pbf"), { config, process: profile, output: join(dir, out) });
      expect(stats.tiles).toBeGreaterThan(5);
      expect(existsSync(join(dir, out))).toBe(true);
    }
    const pm = readFileSync(join(dir, "town.pmtiles"));
    expect(pm.subarray(0, 7).toString()).toBe("PMTiles");
    expect(pm[7]).toBe(3);
    expect(readdirSync(join(dir, "town"))).toContain("12");
    expect(JSON.parse(readFileSync(join(dir, "town", "metadata.json"), "utf8")).name).toBe("Town");
    let sqlite: typeof import("node:sqlite") | undefined;
    try {
      sqlite = await import("node:sqlite");
    } catch {
      sqlite = undefined;
    }
    if (sqlite) {
      const db = new sqlite.DatabaseSync(join(dir, "town.mbtiles"));
      const row = db.prepare("SELECT count(*) AS n FROM tiles").get() as { n: number };
      expect(row.n).toBeGreaterThan(5);
      const name = db.prepare("SELECT value FROM metadata WHERE name = 'name'").get() as { value: string };
      expect(name.value).toBe("Town");
      db.close();
    }
  });

  it("limits tiles to a bbox and gives the same tiles on any thread count", async () => {
    const all = await generateTiles(fixture("town.osm.pbf"), { config, process: profile, threads: 1 }).toArray();
    const again = await generateTiles(fixture("town.osm.pbf"), { config, process: profile, threads: 6 }).toArray();
    expect(again.map((t) => `${t.z}/${t.x}/${t.y}:${t.data.toString("base64")}`)).toEqual(
      all.map((t) => `${t.z}/${t.x}/${t.y}:${t.data.toString("base64")}`),
    );
    const near = await generateTiles(fixture("town.osm.pbf"), { config, process: profile, bbox: [9, 9, 11, 11] }).toArray();
    // Only the bakery's tiles at high zoom remain.
    expect(near.length).toBeLessThan(all.length);
    expect(near.filter((t) => t.z === 12).every((t) => decode(t).layers["poi"]?.feature(0).properties["class"] === "bakery")).toBe(
      true,
    );
    await expect(generateTiles(fixture("town.osm.pbf"), { config, process: profile, bbox: [1, 1, 0, 0] }).toArray()).rejects.toThrow(
      /bbox/,
    );
  });

  it("aborts while reading", async () => {
    const ac = new AbortController();
    const stream = generateTiles(fixture("grid.osm.pbf"), {
      config,
      process: profile,
      signal: ac.signal,
      onProgress: (p) => {
        if (p.phase === "nodes") ac.abort(new Error("stop now"));
      },
    });
    await expect(stream.toArray()).rejects.toThrow("stop now");
  });

  it("reads config and profile from files", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tilemaker-node-files-"));
    const { writeFileSync } = await import("node:fs");
    writeFileSync(join(dir, "config.json"), JSON.stringify(config));
    writeFileSync(join(dir, "process.lua"), profile.source);
    const tiles = await generateTiles(fixture("town.osm.pbf"), {
      config: join(dir, "config.json"),
      process: join(dir, "process.lua"),
    }).toArray();
    expect(tiles.length).toBeGreaterThan(5);
  });

  it("reports errors", async () => {
    await expect(generateTiles(fixture("town.osm.pbf"), { config: { settings: {} }, process: profile }).toArray()).rejects.toThrow(
      /config/,
    );
    await expect(
      generateTiles(fixture("town.osm.pbf"), { config, process: { source: "function node_function() Layer('nope') end" } }).toArray(),
    ).rejects.toThrow(/nope/);
    await expect(generateTiles(fixture("town.osm.pbf"), { config, process: "/nonexistent/process.lua" }).toArray()).rejects.toThrow(
      /nonexistent/,
    );
    await expect(
      writeTiles(fixture("town.osm.pbf"), { config, process: profile, output: "/nonexistent/dir/x.mbtiles" }),
    ).rejects.toThrow();
  });

  it("runs the OpenMapTiles profile on a real extract", async () => {
    const monaco = extract("monaco.osm.pbf");
    if (!monaco) return;
    const dir = mkdtempSync(join(tmpdir(), "tilemaker-node-omt-"));
    const stats = await writeTiles(monaco, {
      config: join(resources, "config-openmaptiles.json"),
      process: join(resources, "process-openmaptiles.lua"),
      output: join(dir, "monaco.pmtiles"),
    });
    expect(stats.tiles).toBeGreaterThanOrEqual(25);
    expect(stats.read.objects).toBeGreaterThan(15_000);
    expect(stats.metadata.vector_layers.map((l) => l.id)).toContain("transportation");
  });
});
