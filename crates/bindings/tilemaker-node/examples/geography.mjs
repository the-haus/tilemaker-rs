// Countries, regions, cities and towns, straight from a planet (or any
// extract): what osmium tags-filter + osmium export + a GeoJSON parse would
// give, in one streaming pass that never builds huge JSON strings.
//
//   node examples/geography.mjs planet.osm.pbf > geography.json

import { bbox, openPbf } from "@tilemaker-rs/node";

const [input] = process.argv.slice(2);
if (!input) {
  console.error("usage: node examples/geography.mjs <file.osm.pbf>");
  process.exit(1);
}

const pbf = await openPbf(input);
const stream = pbf.features({
  filters: ["n/place=city,town", "r/admin_level=2,4"],
  geometryTypes: ["point", "polygon"],
  tags: ["name", "name:en", "place", "admin_level", "ISO3166-1", "ISO3166-2", "wikidata", "population"],
  onProgress: (p) => process.stderr.write(`\r${p.phase} ${p.blocksDone}/${p.blocksTotal}   `),
});

const boundaries = [];
const settlements = [];
for await (const f of stream) {
  if (f.geometry.type === "Point") {
    const [lon, lat] = f.geometry.coordinates;
    settlements.push({ osm: `node/${f.id}`, lon, lat, ...f.tags });
  } else if (f.type === "relation") {
    // Keep a bounding box rather than the (possibly huge) outline.
    boundaries.push({ osm: `relation/${f.id}`, bbox: bbox(f.geometry), points: f.geometry.coordinates.length / 2, ...f.tags });
  }
}
process.stderr.write(`\n${JSON.stringify(await stream.stats)}\n`);
process.stdout.write(JSON.stringify({ boundaries, settlements }) + "\n");
