import { spawnSync } from "node:child_process";
import { createWriteStream, readFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Readable, Writable } from "node:stream";
import { pipeline } from "node:stream/promises";

import { describe, expect, it } from "vitest";

import { geoJSONSeq, openPbf, OsmStreamError, type Feature, type Progress } from "../ts/index.js";
import { fixture } from "./helpers.js";

describe("streaming", () => {
  it("keeps file order across many blocks and batches", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    expect(pbf.blockCount).toBeGreaterThan(10);
    const stream = pbf.features({ batchSize: 1000, threads: 4 });
    let count = 0;
    let lastNode = 0;
    let lastRelation = 0;
    let batches = 0;
    for await (const batch of stream.batches()) {
      batches++;
      expect(batch.length).toBeLessThanOrEqual(1000);
      for (const f of batch) {
        count++;
        if (f.type === "node") {
          expect(f.id).toBeGreaterThan(lastNode);
          lastNode = f.id;
        }
        if (f.type === "relation") {
          expect(f.id).toBeGreaterThan(lastRelation);
          lastRelation = f.id;
          expect(f.geometry.type).toBe("MultiPolygon");
        }
      }
    }
    expect(count).toBe(90_000 + 300 + 29 * 29);
    expect(batches).toBeGreaterThanOrEqual(91);
    const stats = await stream.stats;
    expect(stats.polygons).toBe(841);
    expect(stats.lines).toBe(300);
  });

  it("stops when the loop breaks", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    const stream = pbf.features({ batchSize: 10 });
    let n = 0;
    for await (const f of stream) {
      void f;
      if (++n === 25) break;
    }
    const stats = await stream.stats;
    expect(stats.stopped).toBe(true);
    expect(stats.emitted).toBeLessThan(90_000);
  });

  it("closes before starting", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    const stream = pbf.features();
    stream.close();
    expect(await stream.toArray()).toEqual([]);
    expect((await stream.stats).stopped).toBe(true);
  });

  it("can only be iterated once", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    const stream = pbf.features();
    await stream.toArray();
    await expect(stream.toArray()).rejects.toBeInstanceOf(OsmStreamError);
  });

  it("aborts with a signal", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    const ac = new AbortController();
    const stream = pbf.features({ batchSize: 100, signal: ac.signal });
    let n = 0;
    await expect(
      (async () => {
        for await (const f of stream) {
          void f;
          if (++n === 150) ac.abort(new Error("enough"));
        }
      })(),
    ).rejects.toThrow("enough");
    await expect(stream.stats).rejects.toThrow("enough");
    const pre = new AbortController();
    pre.abort();
    await expect(pbf.features({ signal: pre.signal }).toArray()).rejects.toThrow();
  });

  it("does not run ahead of a slow consumer", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    const progress: Progress[] = [];
    const stream = pbf.features({
      geometryTypes: ["point"],
      batchSize: 100,
      readAhead: 1,
      threads: 2,
      onProgress: (p) => progress.push(p),
    });
    const it = stream[Symbol.asyncIterator]();
    await it.next();
    await new Promise((r) => setTimeout(r, 300));
    // Each node block holds 8000 points = 80 batches; with one batch of
    // read-ahead the producer cannot have finished the first block.
    const done = Math.max(0, ...progress.filter((p) => p.phase === "nodes").map((p) => p.blocksDone));
    expect(done).toBeLessThanOrEqual(1);
    await it.return?.();
    expect((await stream.stats).stopped).toBe(true);
  });

  it("runs several streams at once", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    const [a, b, c] = await Promise.all([
      pbf.features({ geometryTypes: ["polygon"] }).toArray(),
      pbf.elements({ types: ["way"] }).toArray(),
      pbf.features({ filters: ["n/ref=1:*"] }).toArray(),
    ]);
    expect(a).toHaveLength(841);
    expect(b).toHaveLength(300);
    // Only row 1 has refs starting with "1:" ("10:…" does not).
    expect(c).toHaveLength(300);
    expect(c.every((f) => f.tags.ref!.startsWith("1:"))).toBe(true);
  });

  it("maps and filters lazily", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    const out: string[] = [];
    for await (const s of pbf.features().map((f) => `${f.type}/${f.id}`)) out.push(s);
    expect(out).toHaveLength(7);
    const ways: Feature[] = [];
    for await (const f of (await openPbf(fixture("town.osm.pbf"))).features().filter((f) => f.type === "way")) ways.push(f);
    expect(ways).toHaveLength(3);
  });

  it("pipes into Node streams with backpressure", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    let n = 0;
    await pipeline(
      pbf.features({ geometryTypes: ["polygon"] }).toReadable(),
      new Writable({
        objectMode: true,
        highWaterMark: 4,
        write(_f: Feature, _enc, cb) {
          n++;
          setImmediate(cb);
        },
      }),
    );
    expect(n).toBe(841);
  });

  it("writes GeoJSON lines", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tilemaker-node-"));
    const out = join(dir, "town.geojsonseq");
    const pbf = await openPbf(fixture("town.osm.pbf"));
    await pipeline(Readable.from(geoJSONSeq(pbf.features())), createWriteStream(out));
    const lines = readFileSync(out, "utf8").trim().split("\n");
    expect(lines).toHaveLength(7);
    expect(JSON.parse(lines[6]!).id).toBe("relation/200");
  });

  it("surfaces errors thrown by the consumer and stops", async () => {
    const pbf = await openPbf(fixture("grid.osm.pbf"));
    const stream = pbf.features({ batchSize: 10 });
    await expect(
      (async () => {
        for await (const f of stream) {
          void f;
          throw new Error("consumer failed");
        }
      })(),
    ).rejects.toThrow("consumer failed");
    expect((await stream.stats).stopped).toBe(true);
  });

  it("lets the process exit after a stream ends or is abandoned", () => {
    const entry = resolve(__dirname, "..", "dist", "index.js");
    const script = `
      const { openPbf } = require(${JSON.stringify(entry)});
      (async () => {
        const pbf = await openPbf(${JSON.stringify(fixture("grid.osm.pbf"))});
        for await (const f of pbf.features({ batchSize: 5 })) { break; }
        const all = await pbf.features({ geometryTypes: ["polygon"] }).toArray();
        console.log(all.length);
        // An unconsumed, never-started stream must not keep the loop alive either.
        pbf.features();
        // Nor one read half-way and abandoned without close().
        const it = pbf.features({ batchSize: 5, readAhead: 1 })[Symbol.asyncIterator]();
        await it.next();
        // A closed stream still delivers its stats before the process exits.
        const closed = pbf.features({ batchSize: 5 });
        for await (const f of closed) { break; }
        console.log((await closed.stats).stopped);
      })();
    `;
    const r = spawnSync(process.execPath, ["-e", script], { encoding: "utf8", timeout: 30_000 });
    expect(r.error).toBeUndefined();
    expect(r.status).toBe(0);
    expect(r.stdout.trim().split("\n")).toEqual(["841", "true"]);
  });
});
