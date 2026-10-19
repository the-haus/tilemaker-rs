// Vector tiles with a tilemaker profile, written to a PMTiles/MBTiles file
// (or streamed, with --stream, to show each tile as it is finished).
//
//   node examples/tiles.mjs region.osm.pbf region.pmtiles config.json process.lua
//   node examples/tiles.mjs region.osm.pbf --stream config.json process.lua

import { generateTiles, writeTiles } from "@tilemaker-rs/node";

const [input, output, config = "config.json", processLua = "process.lua"] = process.argv.slice(2);
if (!input || !output) {
  console.error("usage: node examples/tiles.mjs <in.osm.pbf> <out.pmtiles|out.mbtiles|dir|--stream> [config.json] [process.lua]");
  process.exit(1);
}
const onProgress = (p) =>
  process.stderr.write(p.phase === "tiles" ? `\rz${p.zoom}: ${p.tilesDone}/${p.tilesTotal}   ` : `\r${p.phase}: ${p.blocksDone}/${p.blocksTotal}   `);

if (output === "--stream") {
  let bytes = 0;
  const tiles = generateTiles(input, { config, process: processLua, compression: "gzip", onProgress });
  for await (const t of tiles) bytes += t.data.length;
  const stats = await tiles.stats;
  console.error(`\n${stats.tiles} tiles, ${bytes} bytes gzipped`);
} else {
  const stats = await writeTiles(input, { config, process: processLua, output, onProgress });
  console.error(`\n${stats.tiles} tiles in ${stats.elapsedMs} ms; layers: ${stats.metadata.vector_layers.map((l) => l.id).join(", ")}`);
}
