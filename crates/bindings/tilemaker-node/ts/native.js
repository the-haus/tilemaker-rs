// Native module loader. Picks the platform-specific prebuilt binary if
// present, otherwise the dev build produced by `napi build` at the crate
// root.
//
// napi-rs appends an ABI suffix on some platforms:
//   - Linux GNU:   tilemaker-node.linux-x64-gnu.node
//   - Linux musl:  tilemaker-node.linux-x64-musl.node
//   - Windows:     tilemaker-node.win32-x64-msvc.node
//   - macOS:       tilemaker-node.darwin-arm64.node  (no ABI suffix)
//
// Published installs resolve the binary through an optional platform
// subpackage named `@tilemaker-rs/node-<triple>`.

"use strict";

// Intentional CJS: `.node` addons are loaded with require().
/* eslint-disable @typescript-eslint/no-require-imports */

const { platform, arch } = process;
const { existsSync } = require("node:fs");
const { join } = require("node:path");

// A glibc build of Node reports the glibc it runs on; a musl build does not.
// (`ldd --version` is no help: musl's prints to stderr and exits 1.)
function isMuslLibc() {
  try {
    const header = process.report?.getReport()?.header;
    if (header) return !header.glibcVersionRuntime;
  } catch {
    /* ignore */
  }
  try {
    return require("node:fs").readFileSync("/usr/bin/ldd", "utf8").includes("musl");
  } catch {
    return false;
  }
}

function candidateTriples() {
  if (platform === "linux") {
    const libc = isMuslLibc() ? "musl" : "gnu";
    return [`${platform}-${arch}-${libc}`, `${platform}-${arch}`];
  }
  if (platform === "win32") {
    return [`${platform}-${arch}-msvc`, `${platform}-${arch}`];
  }
  return [`${platform}-${arch}`];
}

function loadNative() {
  const triples = candidateTriples();
  const localCandidates = [...triples.map((t) => `tilemaker-node.${t}.node`), "tilemaker-node.node"];
  for (const name of localCandidates) {
    const candidate = join(__dirname, "..", name);
    if (existsSync(candidate)) {
      return require(candidate);
    }
  }
  const subpackageNames = triples.map((t) => `@tilemaker-rs/node-${t}`);
  for (const name of subpackageNames) {
    try {
      return require(name);
    } catch (err) {
      if (err && err.code !== "MODULE_NOT_FOUND") throw err;
    }
  }
  throw new Error(
    `@tilemaker-rs/node: no native binary found for ${platform}-${arch}. ` +
      `Tried local: ${localCandidates.join(", ")}. ` +
      `Tried npm: ${subpackageNames.join(", ")}. ` +
      "For local development run `yarn build` in crates/bindings/tilemaker-node.",
  );
}

module.exports = loadNative();
