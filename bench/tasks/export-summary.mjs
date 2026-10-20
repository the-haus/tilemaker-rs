// Export features with the Node API and write one summary line per feature
// (the format of fixtures/planet/*.tsv), reporting time and peak memory.
//
//   node bench/export-summary.mjs <file.osm.pbf> <out.tsv> [--filter EXPR]... [--types point,polygon] [--closed-ways both]

import { createWriteStream } from "node:fs";
import { once } from "node:events";

import { openPbf, polygonRings } from "../dist/index.js";

const args = process.argv.slice(2);
const [input, output] = args;
const filters = [];
let geometryTypes;
let closedWays;
let nodeStoreFile;
for (let i = 2; i < args.length; i++) {
  if (args[i] === "--filter") filters.push(args[++i]);
  else if (args[i] === "--types") geometryTypes = args[++i].split(",");
  else if (args[i] === "--closed-ways") closedWays = args[++i];
  else if (args[i] === "--node-store-file") nodeStoreFile = args[++i];
}

const ringArea = (c, s, e) => {
  let a = 0;
  for (let i = s; i < e - 1; i++) a += c[2 * i] * c[2 * i + 3] - c[2 * i + 2] * c[2 * i + 1];
  return a / 2;
};

const started = Date.now();
let peakRss = 0;
const timer = setInterval(() => (peakRss = Math.max(peakRss, process.memoryUsage().rss)), 250);
const pbf = await openPbf(input);
console.error(`opened ${pbf.blockCount} blocks in ${Date.now() - started} ms`);
const out = createWriteStream(output);
let lastPhase = "";
const stream = pbf.features({
  filters,
  ...(geometryTypes ? { geometryTypes } : {}),
  ...(closedWays ? { closedWays } : {}),
  ...(nodeStoreFile ? { nodeStoreFile } : {}),
  tags: ["admin_level", "boundary", "place"],
  onProgress: (p) => {
    if (p.phase !== lastPhase || p.blocksDone === p.blocksTotal) {
      console.error(`${((Date.now() - started) / 1000).toFixed(1)}s ${p.phase} ${p.blocksDone}/${p.blocksTotal} rss=${Math.round(process.memoryUsage().rss / 1e6)}MB heap=${Math.round(process.memoryUsage().heapUsed / 1e6)}MB ext=${Math.round(process.memoryUsage().arrayBuffers / 1e6)}MB`);
      lastPhase = p.phase;
    }
  },
});
for await (const batch of stream.batches()) {
  let text = "";
  for (const f of batch) {
    const g = f.geometry;
    let polys = 0, rings = 0, pts = 0, area = 0;
    if (g.type === "MultiPolygon") {
      const pr = polygonRings(g);
      polys = pr.length;
      rings = g.parts.length;
      pts = g.coordinates.length / 2;
      for (const p of pr) {
        for (let k = 0; k < p.length; k++) {
          const a = Math.abs(ringArea(g.coordinates, p[k][0], p[k][1]));
          area += k === 0 ? a : -a;
        }
      }
    } else if (g.type === "Point") {
      pts = 1;
    }
    text += `${f.type}/${f.id}\t${g.type}\t${polys}\t${rings}\t${pts}\t${area}\t${f.tags.admin_level ?? ""}\t${f.tags.boundary ?? ""}\t${f.tags.place ?? ""}\n`;
  }
  if (!out.write(text)) await once(out, "drain");
}
out.end();
await once(out, "finish");
clearInterval(timer);
const stats = await stream.stats;
console.error(JSON.stringify({ ...stats, wallMs: Date.now() - started, peakRssMB: Math.round(peakRss / 1e6) }));
