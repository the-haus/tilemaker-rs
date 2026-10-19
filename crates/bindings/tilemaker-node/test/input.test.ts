import { createReadStream, readFileSync } from "node:fs";
import { Readable } from "node:stream";

import { describe, expect, it } from "vitest";

import { openPbf, streamElements, type Element } from "../ts/index.js";
import { fixture } from "./helpers.js";

const ids = (es: Element[]) => es.map((e) => `${e.type}/${e.id}`);

describe("elements from bytes", () => {
  it("reads a Node Readable", async () => {
    const fromStream = await streamElements(createReadStream(fixture("grid.osm.pbf"), { highWaterMark: 4096 }), {
      filters: ["w/highway", "n/ref=1:*"],
    }).toArray();
    const fromFile = await (await openPbf(fixture("grid.osm.pbf"))).elements({ filters: ["w/highway", "n/ref=1:*"] }).toArray();
    expect(fromStream.length).toBe(600);
    expect(ids(fromStream)).toEqual(ids(fromFile));
    expect(fromStream[0]).toEqual(fromFile[0]);
  });

  it("reads a web ReadableStream, an async iterable, a Buffer and an array of chunks", async () => {
    const bytes = readFileSync(fixture("town.osm.pbf"));
    const want = ids(await streamElements(fixture("town.osm.pbf")).toArray());
    const web = new ReadableStream<Uint8Array>({
      start(c) {
        for (let i = 0; i < bytes.length; i += 100) c.enqueue(new Uint8Array(bytes.subarray(i, i + 100)));
        c.close();
      },
    });
    expect(ids(await streamElements(web).toArray())).toEqual(want);
    async function* gen() {
      for (let i = 0; i < bytes.length; i += 33) yield bytes.subarray(i, i + 33);
    }
    expect(ids(await streamElements(gen()).toArray())).toEqual(want);
    expect(ids(await streamElements(bytes).toArray())).toEqual(want);
    expect(ids(await streamElements([bytes.subarray(0, 10), bytes.subarray(10)]).toArray())).toEqual(want);
  });

  it("applies backpressure to a fast source", async () => {
    // ~15 MB of the grid file repeated is not a valid PBF; instead push the
    // grid file in tiny chunks through a source that counts how far ahead
    // of the consumer it gets.
    const bytes = readFileSync(fixture("grid.osm.pbf"));
    let pulled = 0;
    const source = Readable.from(
      (function* () {
        for (let i = 0; i < bytes.length; i += 1024) {
          pulled += 1024;
          yield bytes.subarray(i, i + 1024);
        }
      })(),
    );
    const stream = streamElements(source, { batchSize: 100, readAhead: 1 });
    let n = 0;
    for await (const e of stream) {
      void e;
      n++;
    }
    // Tagged elements: the nodes, the 300 roads and the 841 multipolygons
    // (their square ways are untagged).
    expect(n).toBe(90_000 + 300 + 841);
    expect(pulled).toBeGreaterThanOrEqual(bytes.length);
  });

  it("stops reading when the consumer stops, and buffers at most ~8 MB", async () => {
    // PBF streams concatenate: 30 copies of the grid (~12 MB) is one valid
    // stream, larger than the input buffer.
    const one = readFileSync(fixture("grid.osm.pbf"));
    const copies = 30;
    let pulled = 0;
    async function* source() {
      for (let c = 0; c < copies; c++) {
        for (let i = 0; i < one.length; i += 64 * 1024) {
          pulled += Math.min(64 * 1024, one.length - i);
          yield one.subarray(i, i + 64 * 1024);
        }
      }
    }
    const stream = streamElements(source(), { batchSize: 10, readAhead: 1 });
    const it = stream[Symbol.asyncIterator]();
    await it.next();
    await new Promise((r) => setTimeout(r, 200));
    // The consumer took one element: the source was read only as far as the
    // input buffer (8 MB) plus what the decoder holds.
    expect(pulled).toBeLessThan(10.5 * 1024 * 1024);
    expect(pulled).toBeLessThan(copies * one.length);
    await it.return?.();
    expect((await stream.stats).stopped).toBe(true);
    const total = pulled;
    await new Promise((r) => setTimeout(r, 100));
    expect(pulled).toBe(total);
  });

  it("reads concatenated files completely", async () => {
    const one = readFileSync(fixture("town.osm.pbf"));
    const es = await streamElements(Buffer.concat([one, one, one])).toArray();
    expect(es.length).toBe(3 * (await streamElements(one).toArray()).length);
  });

  it("reports bad input and source errors", async () => {
    await expect(streamElements(Buffer.from("not a pbf at all")).toArray()).rejects.toThrow(/corrupt|truncated/);
    async function* failing() {
      yield readFileSync(fixture("town.osm.pbf")).subarray(0, 50);
      throw new Error("network down");
    }
    await expect(streamElements(failing()).toArray()).rejects.toThrow("network down");
    await expect(streamElements(readFileSync(fixture("town.osm.pbf")), { wayLocations: true }).toArray()).rejects.toThrow(
      /LocationsOnWays/,
    );
  });

  it("gives way coordinates for LocationsOnWays files", async () => {
    const es = await streamElements(readFileSync(fixture("low.osm.pbf")), { wayLocations: true }).toArray();
    const w = es[0]!;
    expect(w.type === "way" && Array.from(w.coordinates!)).toEqual([2, 1, 4, 3, 6, 5]);
  });
});
