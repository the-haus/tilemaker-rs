import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { openPbf, streamFeatures, toGeoJSON, writeTiles, type Feature } from "../ts/index.js";
import { fixture } from "./helpers.js";

describe("robustness", () => {
  it("fails the stream, not the process, when onProgress throws", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    const stream = pbf.features({
      onProgress: () => {
        throw new Error("progress handler broke");
      },
    });
    await expect(stream.toArray()).rejects.toThrow("progress handler broke");
    await expect(stream.stats).rejects.toThrow("progress handler broke");
  });

  it("honours an abort that happens while the stream is starting", async () => {
    const ac = new AbortController();
    const stream = streamFeatures(fixture("grid.osm.pbf"), { signal: ac.signal });
    const pending = stream.toArray();
    ac.abort(new Error("changed my mind")); // while the file is being opened
    await expect(pending).rejects.toThrow("changed my mind");

    const dir = mkdtempSync(join(tmpdir(), "tilemaker-node-abort-"));
    const out = join(dir, "never.pmtiles");
    const ac2 = new AbortController();
    const writing = writeTiles(fixture("grid.osm.pbf"), {
      config: {
        layers: { poi: { minzoom: 0, maxzoom: 4 } },
        settings: { minzoom: 0, maxzoom: 4, basezoom: 4, name: "x", version: "1", description: "", compress: "gzip" },
      },
      process: { source: "function node_function() Layer('poi') end" },
      output: out,
      signal: ac2.signal,
    });
    ac2.abort(new Error("stop writing")); // while the config is being prepared
    await expect(writing).rejects.toThrow("stop writing");
    expect(existsSync(out)).toBe(false);
  });

  it("closes streams that are dropped unfinished once collected", () => {
    const entry = resolve(__dirname, "..", "dist", "index.js");
    const script = `
      const { openPbf } = require(${JSON.stringify(entry)});
      (async () => {
        const pbf = await openPbf(${JSON.stringify(fixture("grid.osm.pbf"))});
        for (let i = 0; i < 5; i++) {
          const it = pbf.features({ batchSize: 10, readAhead: 1 })[Symbol.asyncIterator]();
          await it.next();
        }
        for (let i = 0; i < 10; i++) { global.gc(); await new Promise((r) => setTimeout(r, 20)); }
        const all = await pbf.features({ geometryTypes: ["polygon"] }).toArray();
        console.log(all.length);
      })();
    `;
    const r = spawnSync(process.execPath, ["--expose-gc", "-e", script], { encoding: "utf8", timeout: 30_000 });
    expect(r.stderr).toBe("");
    expect(r.status).toBe(0);
    expect(r.stdout.trim()).toBe("841");
  });

  it("keeps streams that are being awaited alive through garbage collection", () => {
    const entry = resolve(__dirname, "..", "dist", "index.js");
    // Collect garbage constantly while consumers wait for batches: an
    // awaited stream is reachable only through its own pending promise.
    const script = `
      const { openPbf, streamElements } = require(${JSON.stringify(entry)});
      const { createReadStream } = require("node:fs");
      const timer = setInterval(() => global.gc(), 1);
      (async () => {
        const pbf = await openPbf(${JSON.stringify(fixture("grid.osm.pbf"))});
        let total = 0;
        for (let i = 0; i < 2; i++) {
          total += (await pbf.features({ batchSize: 50, readAhead: 1 }).toArray()).length;
          total += (await streamElements(createReadStream(${JSON.stringify(fixture("grid.osm.pbf"))}, { highWaterMark: 4096 }), { batchSize: 50 }).toArray()).length;
        }
        clearInterval(timer);
        console.log(total);
      })();
    `;
    const r = spawnSync(process.execPath, ["--expose-gc", "-e", script], { encoding: "utf8", timeout: 60_000 });
    expect(r.error).toBeUndefined();
    expect(r.stderr).toBe("");
    expect(r.stdout.trim()).toBe(String(2 * (91_141 + 91_141)));
  });

  it("keeps odd tag keys in GeoJSON", () => {
    const f: Feature = {
      type: "node",
      id: 7,
      tags: Object.defineProperty({ "@id": "fake", name: "x" } as Record<string, string>, "__proto__", {
        value: "p",
        enumerable: true,
      }),
      geometry: { type: "Point", coordinates: new Float64Array([1, 2]) },
    };
    const g = toGeoJSON(f);
    expect(g.properties["@id"]).toBe(7);
    expect(Object.keys(g.properties)).toContain("__proto__");
    expect(Object.getOwnPropertyDescriptor(g.properties, "__proto__")?.value).toBe("p");
    expect(JSON.parse(JSON.stringify(g)).properties.__proto__).toBe("p");
  });
});
