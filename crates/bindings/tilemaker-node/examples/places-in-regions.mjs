// Cities and towns with the country and region they lie in — the join
// festimap used DuckDB's spatial extension for — from one PBF file:
// the areas go into a PolygonIndex, then every place is looked up.
//
//   node examples/places-in-regions.mjs planet.osm.pbf > places.json

import { openPbf, PolygonIndex } from "@tilemaker-rs/node";

const [input] = process.argv.slice(2);
if (!input) {
  console.error("usage: node examples/places-in-regions.mjs <file.osm.pbf>");
  process.exit(1);
}

const tags = ["name", "admin_level", "ISO3166-1", "ISO3166-2"];
// Countries and regions are relations (a closed border way can carry the
// same tags, so ways are left out).
const areas = await PolygonIndex.fromFile(input, { filters: ["r/admin_level=2,4"], tags });
const pbf = await openPbf(input);
const places = await pbf.features({ filters: ["n/place=city,town"], tags: ["name", "place", "population"] }).toArray();

const at = (p, level) => {
  const [lon, lat] = p.geometry.coordinates;
  const a = areas.find(lon, lat, (e) => e.tags.admin_level === level);
  return a ? `relation/${a.id}` : null;
};
const settlements = places.map((p) => ({
  osm: `node/${p.id}`,
  lon: p.geometry.coordinates[0],
  lat: p.geometry.coordinates[1],
  ...p.tags,
  country: at(p, "2"),
  region: at(p, "4"),
}));
const placed = settlements.filter((s) => s.country).length;
process.stderr.write(`${areas.size} areas, ${settlements.length} places, ${placed} in a country\n`);
process.stdout.write(JSON.stringify(settlements) + "\n");
