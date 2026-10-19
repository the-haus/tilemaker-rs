# Fixtures

Real OpenStreetMap extracts used by the comparison tests. The `.osm.pbf`
files are not committed; tests that need them skip when they are absent.

They were cut from the planet of 2026-10-12 with osmium:

```sh
osmium extract -c extracts.json -s complete_ways --set-bounds planet.osm.pbf
```

`ref/` holds what the originals produced from them:

- `monaco-areas.json`, `liechtenstein-admin.json`: per relation, the
  polygon, ring and point counts and the area of `osmium export`'s output
  (for `r/type=multipolygon,boundary` and `r/admin_level=2,4,6,8`).
- `*-openmaptiles-cpp.json`: `tilemaker inspect --json` of the C++
  tilemaker's output with `resources/config-openmaptiles.json` and
  `resources/process-openmaptiles.lua`.

Extracts cut from a different planet will differ slightly; regenerate the
references then (the C++ tilemaker builds with CMake; see its README).
