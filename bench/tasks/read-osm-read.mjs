// Read every element with osm-read (filtering in JS when asked).
//   node read-osm-read.mjs <file.osm.pbf> [key]
import osmread from "osm-read";

const [path, filter] = process.argv.slice(2);
const [type, key] = filter ? filter.split("/") : [];
const counts = { node: 0, way: 0, relation: 0, tags: 0 };
const see = (t) => (e) => {
  if (filter && (t[0] !== type || !(key in e.tags))) return;
  counts[t]++;
  for (const _ in e.tags) counts.tags++;
};
await new Promise((resolve, reject) =>
  osmread.parse({
    filePath: path,
    format: "pbf",
    node: see("node"),
    way: see("way"),
    relation: see("relation"),
    endDocument: resolve,
    error: reject,
  }),
);
console.log(JSON.stringify(counts));
