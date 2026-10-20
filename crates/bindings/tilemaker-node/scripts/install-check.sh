#!/bin/sh
# Install the packed @tilemaker-rs/node from packages/ (see release-node.yml)
# into an empty project, with the platform package for one triple, and use it
# from ESM and CommonJS. Run from the repository root:
#
#   sh crates/bindings/tilemaker-node/scripts/install-check.sh linux-x64-gnu
set -eu
TRIPLE="$1"
rm -rf install-check
mkdir install-check
cd install-check
npm init -y >/dev/null
# Relative paths, which npm on Windows understands too. In a dry run the
# other platform packages are not on the registry; npm skips optional
# dependencies it cannot fetch.
npm install --no-audit --no-fund ../packages/tilemaker-rs-node-[0-9]*.tgz "../packages/tilemaker-rs-node-$TRIPLE-"*.tgz
ls node_modules/@tilemaker-rs
cp ../crates/bindings/tilemaker-node/test/fixtures/town.osm.pbf .
cat > esm.mjs <<'JS'
import { openPbf, version } from "@tilemaker-rs/node";
const pbf = await openPbf("town.osm.pbf");
const n = (await pbf.features({ filters: ["n/amenity"] }).toArray()).length;
if (n === 0) throw new Error("no features");
console.log(`esm: ${version()} on ${process.platform}-${process.arch}, node ${process.version}: ${n} amenities`);
JS
cat > cjs.cjs <<'JS'
const { streamElements } = require("@tilemaker-rs/node");
(async () => {
  let n = 0;
  for await (const e of streamElements("town.osm.pbf", { types: ["way"] })) n++;
  if (n === 0) throw new Error("no ways");
  console.log(`cjs: ${n} ways`);
})();
JS
node esm.mjs
node cjs.cjs
