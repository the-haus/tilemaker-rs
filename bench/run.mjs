// Run the benchmarks: wall time and peak RSS (from /usr/bin/time) of each
// tool on each dataset, written to results.json and printed as Markdown.
//
//   node run.mjs [--group read,filter,count,geography,extract,within,tiles]
//                [--datasets monaco,liechtenstein,netherlands,geography,planet]
//                [--tools tilemaker,osmium,...] [--timeout 1800] [--report]
//
// Datasets: fixtures/*.osm.pbf; geography and planet from
// $TILEMAKER_BENCH_GEOGRAPHY and $TILEMAKER_BENCH_PLANET. Tools: our CLI
// (target/release/tilemaker, `cargo build --release` first) and Node
// package (`yarn build` first), osmium on $PATH, the C++ tilemaker at
// $TILEMAKER_CPP (default ../../tilemaker-upstream/build/tilemaker).

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const list = (name) => arg(name)?.split(",");
const timeout = Number(arg("timeout", "1800"));
const work = resolve(arg("work", join(here, ".work")));
mkdirSync(work, { recursive: true });

const datasets = {
  monaco: join(root, "fixtures/monaco.osm.pbf"),
  liechtenstein: join(root, "fixtures/liechtenstein.osm.pbf"),
  netherlands: join(root, "fixtures/netherlands.osm.pbf"),
  geography: process.env.TILEMAKER_BENCH_GEOGRAPHY,
  planet: process.env.TILEMAKER_BENCH_PLANET,
};
const cli = join(root, "target/release/tilemaker");
const cpp = process.env.TILEMAKER_CPP ?? resolve(root, "../tilemaker-upstream/build/tilemaker");
const resources = join(root, "resources");
const node = (script, ...a) => ["node", join(here, "tasks", script), ...a];
const out = (name) => join(work, name);
const geography = ["n/place=city,town", "r/admin_level=2,4"];
const amsterdam = "4.7,52.3,5.0,52.45";
const omt = ["--config", join(resources, "config-openmaptiles.json"), "--process", join(resources, "process-openmaptiles.lua")];

// group → dataset list → tool → commands (run in order; times add up).
const tasks = [
  {
    group: "read",
    title: "Every element as a JS object",
    datasets: ["monaco", "liechtenstein", "netherlands"],
    tools: {
      "@tilemaker-rs/node": (f) => [node("read-tilemaker.mjs", f)],
      "osm-pbf-parser": (f) => [node("read-osm-pbf-parser.mjs", f)],
      "osm-read": (f) => [node("read-osm-read.mjs", f)],
    },
  },
  {
    group: "filter",
    title: "Nodes with an amenity tag, as JS objects",
    datasets: ["monaco", "liechtenstein", "netherlands"],
    tools: {
      "@tilemaker-rs/node": (f) => [node("read-tilemaker.mjs", f, "n/amenity")],
      "osm-pbf-parser": (f) => [node("read-osm-pbf-parser.mjs", f, "n/amenity")],
      "osm-read": (f) => [node("read-osm-read.mjs", f, "n/amenity")],
    },
  },
  {
    group: "count",
    title: "File statistics (`fileinfo -e`: counts, ids, bbox, timestamps)",
    datasets: ["monaco", "liechtenstein", "netherlands", "planet"],
    tools: {
      "tilemaker fileinfo": (f) => [[cli, "fileinfo", "-e", f]],
      "osmium fileinfo": (f) => [["osmium", "fileinfo", "-e", f]],
    },
  },
  {
    group: "geography",
    title: "Cities, towns, countries and regions as GeoJSON lines",
    datasets: ["netherlands", "geography", "planet"],
    tools: {
      "tilemaker export": (f) => [[cli, "export", f, ...geography.flatMap((g) => ["-f", g]), "--geometry-types", "point,polygon", "-o", out("geo.geojsonseq")]],
      "Node writeGeoJSONSeq": (f) => [node("geography-tilemaker.mjs", f, "geojson", out("geo-node.geojsonseq"))],
      "Node features() (objects)": (f) => [node("geography-tilemaker.mjs", f, "objects")],
      "osmium tags-filter + export": (f) => [
        ["osmium", "tags-filter", f, ...geography, "-o", out("geo.osm.pbf"), "--overwrite"],
        ...["polygon", "point"].map((g) => [
          "osmium", "export", out("geo.osm.pbf"), "-f", "geojsonseq", "-x", "print_record_separator=false",
          "-a", "type,id", `--geometry-types=${g}`, "-o", out(`osmium-${g}.geojsonseq`), "--overwrite",
        ]),
      ],
    },
  },
  {
    group: "within",
    title: "Cities and towns with their country and region (reverse geocoding)",
    datasets: ["geography", "planet"],
    tools: {
      "tilemaker export --within": (f) => [[cli, "export", f, "-f", "n/place=city,town", "--within", "r/admin_level=2,4", "--tag", "name", "--tag", "admin_level", "-o", out("within.geojsonseq")]],
      "Node enrich.within": (f) => [node("geography-tilemaker.mjs", f, "within")],
    },
  },
  {
    group: "extract",
    title: "PBF extracts",
    datasets: ["netherlands", "planet"],
    variants: {
      "tags-filter geography": {
        "tilemaker extract": (f) => [[cli, "extract", f, ...geography.flatMap((g) => ["-f", g]), "-o", out("x.osm.pbf"), "--overwrite"]],
        "osmium tags-filter": (f) => [["osmium", "tags-filter", f, ...geography, "-o", out("x.osm.pbf"), "--overwrite"]],
      },
      "bbox (Amsterdam)": {
        "tilemaker extract": (f) => [[cli, "extract", f, "-b", amsterdam, "-o", out("x.osm.pbf"), "--overwrite"]],
        "osmium extract": (f) => [["osmium", "extract", "-b", amsterdam, f, "-o", out("x.osm.pbf"), "--overwrite"]],
      },
    },
  },
  {
    group: "tiles",
    title: "Vector tiles, OpenMapTiles profile → MBTiles",
    datasets: ["monaco", "liechtenstein", "netherlands"],
    tools: {
      "tilemaker-rs": (f) => [["rm", "-f", out("t.mbtiles")], [cli, f, out("t.mbtiles"), ...omt, "--quiet"]],
      "C++ tilemaker": (f) => [["rm", "-f", out("t.mbtiles")], [cpp, f, out("t.mbtiles"), ...omt, "--quiet"]],
    },
  },
];

/** Run a command under /usr/bin/time; seconds and peak RSS in MB. */
function measure(cmd) {
  const r = spawnSync("/usr/bin/time", ["-l", ...cmd], {
    cwd: resources,
    encoding: "utf8",
    timeout: timeout * 1000,
    maxBuffer: 1 << 28,
    env: { ...process.env, NODE_OPTIONS: "--max-old-space-size=16384" },
  });
  const err = r.stderr ?? "";
  const real = /([\d.]+) real/.exec(err);
  const rss = /(\d+)\s+maximum resident set size/.exec(err);
  if (r.error?.code === "ETIMEDOUT" || r.signal) return { ok: false, note: `> ${timeout} s` };
  if (r.status !== 0 || !real) return { ok: false, note: err.split("\n").filter((l) => /error|Error/.test(l))[0]?.slice(0, 120) ?? `exit ${r.status}` };
  return { ok: true, seconds: Number(real[1]), rssMB: Math.round(Number(rss?.[1] ?? 0) / 1e6), stdout: r.stdout.trim().slice(0, 300) };
}

// --report: only print the tables of results.json.
const report = process.argv.includes("--report");
const groups = report ? [] : list("group");
const onlyDatasets = list("datasets");
const onlyTools = list("tools");
const resultsPath = join(here, "results.json");
const results = existsSync(resultsPath) ? JSON.parse(readFileSync(resultsPath, "utf8")) : {};
for (const t of tasks) {
  if (groups && !groups.includes(t.group)) continue;
  if (report) break;
  const variants = t.variants ?? { "": t.tools };
  for (const [variant, tools] of Object.entries(variants)) {
    for (const d of t.datasets) {
      if (onlyDatasets && !onlyDatasets.includes(d)) continue;
      const file = datasets[d];
      if (!file || !existsSync(file)) continue;
      if (variant.startsWith("bbox") && d !== "netherlands") continue;
      // Read the file once so that no tool pays for the disk alone (files
      // larger than memory stay cold for everyone).
      if (statSync(file).size < 16e9) spawnSync("cat", [file], { stdio: ["ignore", "ignore", "ignore"] });
      for (const [tool, make] of Object.entries(tools)) {
        if (onlyTools && !onlyTools.some((x) => tool.includes(x))) continue;
        // Quick runs are repeated (best of three): a first run pays for
        // launching a new binary and cold caches.
        let best;
        for (let attempt = 0; attempt < 3; attempt++) {
          let seconds = 0;
          let rssMB = 0;
          let res = { ok: true };
          for (const cmd of make(file)) {
            res = measure(cmd);
            if (!res.ok) break;
            seconds += res.seconds;
            rssMB = Math.max(rssMB, res.rssMB);
          }
          if (!best || (res.ok && seconds < best.seconds)) best = { ...res, seconds, rssMB };
          if (!res.ok || seconds >= 10) break;
        }
        const res = best;
        const { seconds, rssMB } = best;
        const key = [t.group, variant, d, tool].join("|");
        results[key] = res.ok
          ? { group: t.group, variant, dataset: d, tool, seconds: Math.round(seconds * 100) / 100, rssMB, at: new Date().toISOString() }
          : { group: t.group, variant, dataset: d, tool, failed: res.note, at: new Date().toISOString() };
        console.error(`${t.group} ${variant} ${d} ${tool}: ${res.ok ? `${seconds.toFixed(2)} s, ${rssMB} MB` : res.note}`);
        writeFileSync(resultsPath, JSON.stringify(results, null, 2) + "\n");
      }
    }
  }
}
if (!report) rmSync(work, { recursive: true, force: true });

// Markdown: one table per group (and variant), datasets as columns.
const fmt = (r) => (!r ? "" : r.failed ? r.failed : `${r.seconds < 10 ? r.seconds.toFixed(2) : r.seconds.toFixed(1)} s, ${r.rssMB >= 1000 ? `${(r.rssMB / 1000).toFixed(1)} GB` : `${r.rssMB} MB`}`);
const rows = Object.values(results);
for (const t of tasks) {
  for (const variant of Object.keys(t.variants ?? { "": 0 })) {
    const rs = rows.filter((r) => r.group === t.group && r.variant === variant);
    if (!rs.length) continue;
    const ds = t.datasets.filter((d) => rs.some((r) => r.dataset === d));
    const tools = [...new Set(rs.map((r) => r.tool))];
    console.log(`\n#### ${t.title}${variant ? ` — ${variant}` : ""}\n`);
    console.log(`| | ${ds.join(" | ")} |`);
    console.log(`|---|${ds.map(() => "---").join("|")}|`);
    for (const tool of tools) {
      console.log(`| ${tool} | ${ds.map((d) => fmt(rs.find((r) => r.dataset === d && r.tool === tool))).join(" | ")} |`);
    }
  }
}
