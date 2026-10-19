// Points of interest as CSV, transformed on the way out: shops and
// restaurants mapped as nodes or as buildings, each with one point (the
// polygon's label point for areas).
//
//   node examples/pois-csv.mjs region.osm.pbf > pois.csv

import { openPbf, polygonRings } from "@tilemaker-rs/node";

const [input] = process.argv.slice(2);
if (!input) {
  console.error("usage: node examples/pois-csv.mjs <file.osm.pbf>");
  process.exit(1);
}

/** The average of a polygon's exterior vertices: good enough for a label here. */
function center(g) {
  if (g.type === "Point") return [g.coordinates[0], g.coordinates[1]];
  const [start, end] = g.type === "MultiPolygon" ? polygonRings(g)[0][0] : [0, g.coordinates.length / 2];
  let x = 0;
  let y = 0;
  for (let i = start; i < end; i++) {
    x += g.coordinates[2 * i];
    y += g.coordinates[2 * i + 1];
  }
  return [x / (end - start), y / (end - start)];
}

const csv = (v) => (v === undefined ? "" : /[",\n]/.test(v) ? `"${v.replaceAll('"', '""')}"` : v);
const pbf = await openPbf(input);
process.stdout.write("osm,kind,name,lon,lat\n");
for await (const f of pbf.features({ filters: ["shop", "amenity=restaurant,cafe,bar"], tags: ["name", "shop", "amenity"] })) {
  const [lon, lat] = center(f.geometry);
  const kind = f.tags.shop ? `shop=${f.tags.shop}` : `amenity=${f.tags.amenity}`;
  process.stdout.write(`${f.type}/${f.id},${csv(kind)},${csv(f.tags.name)},${lon.toFixed(7)},${lat.toFixed(7)}\n`);
}
