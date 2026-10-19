// The compiled wrapper loads `./native.js` relative to itself; copy the
// loader and its types next to the emitted files in dist/.
import { copyFileSync } from "node:fs";

copyFileSync("ts/native.js", "dist/native.js");
copyFileSync("ts/native.d.ts", "dist/native.d.ts");
