#!/bin/bash
# Pack @tilemaker-rs/node, install the tarball into a fresh project, and use
# it from ESM, CommonJS and TypeScript.
set -euo pipefail
PKG=$(cd "$(dirname "$0")/.." && pwd)
F="$PKG/test/fixtures/town.osm.pbf"
W=$(mktemp -d)
cd "$PKG"
rm -f tilemaker-rs-node-0.1.0.tgz
npm pack --ignore-scripts >/dev/null
mv tilemaker-rs-node-0.1.0.tgz "$W/"
cd "$W"
npm init -y >/dev/null
npm install ./tilemaker-rs-node-0.1.0.tgz typescript@5 @types/node@22 >/dev/null 2>&1
ls node_modules/@tilemaker-rs/node
cat > esm.mjs <<'JS'
import { openPbf, toGeoJSON, version } from "@tilemaker-rs/node";
const pbf = await openPbf(process.argv[2]);
const fs = await pbf.features({ filters: ["n/amenity"] }).toArray();
console.log("esm", version(), fs.length, toGeoJSON(fs[0]).id);
JS
cat > cjs.cjs <<'JS'
const { streamElements } = require("@tilemaker-rs/node");
(async () => { let n = 0; for await (const e of streamElements(process.argv[2], { types: ["way"] })) n++; console.log("cjs", n); })();
JS
cat > app.ts <<'TS'
import { openPbf, type Feature, type MultiPolygonGeometry } from "@tilemaker-rs/node";
async function main(path: string): Promise<number> {
  const pbf = await openPbf(path);
  let n = 0;
  for await (const f of pbf.features({ geometryTypes: ["polygon"] })) {
    const g = f.geometry as MultiPolygonGeometry;
    n += g.polygons.length;
    const typed: Feature = f;
    void typed;
  }
  return n;
}
main(process.argv[2]!).then((n) => console.log("ts", n));
TS
echo '{"compilerOptions":{"module":"NodeNext","moduleResolution":"NodeNext","target":"ES2022","strict":true,"outDir":"out","rootDir":".","types":["node"]}}' > tsconfig.json
node esm.mjs "$F"
node cjs.cjs "$F"
npx tsc -p .
node out/app.js "$F"
rm -rf "$W"
