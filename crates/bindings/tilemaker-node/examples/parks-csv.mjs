// Parks as CSV with their area and a point to label them by, computed
// natively (enrich), coordinates rounded to 6 decimals (precision).
//
//   node examples/parks-csv.mjs region.osm.pbf > parks.csv

import { streamFeatures } from "@tilemaker-rs/node";

const [input] = process.argv.slice(2);
if (!input) {
  console.error("usage: node examples/parks-csv.mjs <file.osm.pbf>");
  process.exit(1);
}

const csv = (v) => (/[",\n]/.test(v) ? `"${v.replaceAll('"', '""')}"` : v);
process.stdout.write("osm,name,area_m2,lon,lat\n");
for await (const f of streamFeatures(input, {
  filters: ["a/leisure=park"],
  geometryTypes: ["polygon"],
  tags: ["name"],
  precision: 6,
  enrich: { area: true, labelPoint: true },
})) {
  const [lon, lat] = f.labelPoint;
  process.stdout.write(`${f.type}/${f.id},${csv(f.tags.name ?? "")},${Math.round(f.area)},${lon.toFixed(6)},${lat.toFixed(6)}\n`);
}
