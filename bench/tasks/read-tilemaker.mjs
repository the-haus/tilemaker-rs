// Read every element into JS objects with @tilemaker-rs/node, or only those
// matching a filter (decided natively). Prints counts.
//   node read-tilemaker.mjs <file.osm.pbf> [filter]
import { streamElements } from "@tilemaker-rs/node";

const [path, filter] = process.argv.slice(2);
const counts = { node: 0, way: 0, relation: 0, tags: 0 };
const options = filter ? { filters: [filter] } : { includeUntagged: true };
for await (const batch of streamElements(path, options).batches()) {
  for (const e of batch) {
    counts[e.type]++;
    for (const _ in e.tags) counts.tags++;
  }
}
console.log(JSON.stringify(counts));
