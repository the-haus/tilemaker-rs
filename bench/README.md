# Benchmarks

`run.mjs` times our CLI and Node package against
[osmium](https://osmcode.org/osmium-tool/), the
[C++ tilemaker](https://github.com/systemed/tilemaker), and the JavaScript
PBF readers [osm-pbf-parser](https://github.com/substack/osm-pbf-parser)
and [osm-read](https://github.com/marook/osm-read) (dev dependencies of
this workspace only). Each run is measured with `/usr/bin/time -l` (wall
time, peak resident memory); runs under 10 s are repeated and the best of
three is kept, and each input file is read once beforehand so no tool pays
for the disk alone (except the planet, which does not fit in memory: every
tool reads it cold).

```sh
cargo build --release -p tilemaker-cli          # target/release/tilemaker
(cd crates/bindings/tilemaker-node && corepack yarn build)
cd bench && corepack yarn install
TILEMAKER_BENCH_GEOGRAPHY=…/geography.osm.pbf TILEMAKER_BENCH_PLANET=…/planet.osm.pbf \
  node run.mjs [--group read,filter,count,geography,within,extract,tiles] [--datasets monaco,netherlands,…]
```

Results go to `results.json` (merged across runs) and are printed as the
tables below. `tasks/export-summary.mjs` writes one summary line per
exported feature (type/id, polygon/ring/point counts, area), for comparing
feature by feature with a summary of `osmium export` output.

Datasets: Monaco (1.6 MB), Liechtenstein (8.7 MB) and the Netherlands with
its surroundings (2.6 GB; 226 M nodes, 33 M ways, 373 k relations), cut
from the planet of 2026-10-12 ([fixtures](../fixtures/README.md));
`geography.osm.pbf` (540 MB: the planet's cities, towns and admin_level 2
and 4 boundaries, made by `osmium tags-filter`); the planet itself (95 GB).

Machine: Apple M1 Max (10 cores), 32 GiB, macOS 26, SSD; osmium 1.19.1, Node 24, C++ tilemaker 3.2.0 (commit facdf93, 2026-09-03).

## Results

<!-- bench:tables -->
