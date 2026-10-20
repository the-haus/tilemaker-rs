// Cut an extract, as osmium tags-filter and/or osmium extract would:
//
//   node examples/extract.mjs in.osm.pbf out.osm.pbf [--bbox w,s,e,n] [--polygon area.geojson] [filter…]
//
// e.g. the highways of a city:
//   node examples/extract.mjs netherlands.osm.pbf amsterdam-roads.osm.pbf --bbox 4.7,52.3,5.0,52.45 w/highway

import { extract, fileInfo } from "@tilemaker-rs/node";

const args = process.argv.slice(2);
const [input, output] = args;
if (!input || !output) {
  console.error("usage: node examples/extract.mjs <in.osm.pbf> <out.osm.pbf> [--bbox w,s,e,n] [--polygon file] [filter…]");
  process.exit(1);
}
const options = { output, filters: [] };
for (let i = 2; i < args.length; i++) {
  if (args[i] === "--bbox") options.bbox = args[++i].split(",").map(Number);
  else if (args[i] === "--polygon") options.polygon = args[++i];
  else options.filters.push(args[i]);
}
if (!options.filters.length) delete options.filters;

const stats = await extract(input, {
  ...options,
  onProgress: (p) => process.stderr.write(`\r${p.phase} ${p.blocksDone}/${p.blocksTotal}   `),
});
process.stderr.write(`\n${stats.nodes} nodes, ${stats.ways} ways, ${stats.relations} relations, ${stats.bytes} bytes\n`);
const info = await fileInfo(output, { extended: true });
process.stdout.write(JSON.stringify({ bbox: info.data.bbox, counts: info.data.counts }) + "\n");
