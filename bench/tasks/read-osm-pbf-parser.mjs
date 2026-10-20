// Read every element with osm-pbf-parser (filtering in JS when asked).
//   node read-osm-pbf-parser.mjs <file.osm.pbf> [key]
import { createReadStream } from "node:fs";
import { Writable } from "node:stream";
import { pipeline } from "node:stream/promises";

import parseOSM from "osm-pbf-parser";

const [path, filter] = process.argv.slice(2);
// "n/amenity" → nodes with the key.
const [type, key] = filter ? filter.split("/") : [];
const counts = { node: 0, way: 0, relation: 0, tags: 0 };
await pipeline(
  createReadStream(path),
  parseOSM(),
  new Writable({
    objectMode: true,
    write(items, _enc, next) {
      for (const e of items) {
        if (filter && (e.type[0] !== type || !(key in e.tags))) continue;
        if (!(e.type in counts)) continue;
        counts[e.type]++;
        for (const _ in e.tags) counts.tags++;
      }
      next();
    },
  }),
);
console.log(JSON.stringify(counts));
