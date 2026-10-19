// Filtered features as newline-delimited GeoJSON (like osmium export).
//
//   node examples/export-geojsonseq.mjs region.osm.pbf out.geojsonseq "amenity=cafe,restaurant" "w/highway"

import { createWriteStream } from "node:fs";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";

import { geoJSONSeq, streamFeatures } from "@tilemaker-rs/node";

const [input, output, ...filters] = process.argv.slice(2);
if (!input || !output) {
  console.error("usage: node examples/export-geojsonseq.mjs <in.osm.pbf> <out.geojsonseq> [filter...]");
  process.exit(1);
}
const features = streamFeatures(input, { filters });
await pipeline(Readable.from(geoJSONSeq(features)), createWriteStream(output));
console.error(JSON.stringify(await features.stats));
