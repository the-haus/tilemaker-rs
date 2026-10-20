// Countries, regions, cities and towns (festimap's geography):
//   node geography-tilemaker.mjs <file.osm.pbf> objects       — as JS objects
//   node geography-tilemaker.mjs <file.osm.pbf> geojson <out> — written natively
//   node geography-tilemaker.mjs <file.osm.pbf> within        — places with their country/region
import { openPbf, streamFeatures, writeGeoJSONSeq } from "@tilemaker-rs/node";

const [path, mode, out] = process.argv.slice(2);
const filters = ["n/place=city,town", "r/admin_level=2,4"];
if (mode === "geojson") {
  const stats = await writeGeoJSONSeq(path, { filters, geometryTypes: ["point", "polygon"], output: out });
  console.log(JSON.stringify({ emitted: stats.emitted }));
} else if (mode === "within") {
  const pbf = await openPbf(path);
  let n = 0;
  let placed = 0;
  for await (const f of pbf.features({
    filters: ["n/place=city,town"],
    tags: ["name", "place", "admin_level"],
    enrich: { within: ["r/admin_level=2,4"] },
  })) {
    n++;
    if (f.within.length) placed++;
  }
  console.log(JSON.stringify({ places: n, placed }));
} else {
  let n = 0;
  let points = 0;
  for await (const f of streamFeatures(path, { filters, geometryTypes: ["point", "polygon"] })) {
    n++;
    points += f.geometry.coordinates.length / 2;
  }
  console.log(JSON.stringify({ features: n, points }));
}
