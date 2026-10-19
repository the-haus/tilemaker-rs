# Benchmarks

`export-summary.mjs` exports features through the Node API and writes one
summary line per feature (type/id, geometry type, polygon/ring/point counts,
area, a few tags), printing the passes with memory use and the final stats.
The same summary of `osmium export` output lets the two be compared
feature by feature.

```sh
yarn build
node bench/export-summary.mjs planet.osm.pbf out.tsv \
  --filter n/place=city,town --filter r/admin_level=2,4 --types point,polygon
```

Results (10-core Apple M-series, planet of 2026-10-12, 95 GB):

| | time | peak RSS |
|---|---|---|
| `osmium tags-filter` + `osmium export` (GeoJSON) | 462 s + 129 s | 3.2 GB / 4.1 GB |
| `features({ filters })` straight from the planet | 244 s | 1.4 GB |

Both produce the same 115,331 cities and towns and the same 3,567
country/region multipolygons (identical rings and points).
