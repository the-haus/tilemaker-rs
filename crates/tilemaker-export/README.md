# tilemaker-export

Stream filtered OpenStreetMap elements and assembled features out of PBF
files, keeping only what the selection needs in memory.

```rust,no_run
use tilemaker_export::*;

let file = PbfFile::open("planet.osm.pbf")?;
let opts = FeatureOptions {
    common: CommonOptions {
        filters: FilterSet::parse(&["n/place=city,town", "r/admin_level=2,4"])?,
        ..Default::default()
    },
    ..Default::default()
};
export_features(&file, &opts, |event| {
    if let Event::Batch(features) = event {
        for f in features {
            println!("{}", f.to_geojson());
        }
    }
    Flow::Continue
})?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Features match `osmium export`: on the world's administrative boundaries
the same relations assemble with identical rings, points and areas. See
the [engine docs](https://github.com/the-haus/tilemaker-rs/blob/main/crates/tilemaker-export/src/engine.rs) for the passes.

Also in this crate:

- `FeatureOptions::{transform, enrich, member_of, within}`: clip,
  simplify, round; area, length, centroid, label point, bbox, parent
  relations and containing areas per feature;
- `write_geojson_seq`: GeoJSON lines serialised on the worker threads;
- `extract`: PBF extracts identical to `osmium tags-filter` / `osmium
  extract` (complete_ways), with box, GeoJSON or `.poly` regions;
- `PolygonIndex`: point-in-polygon lookups (reverse geocoding);
- `file_info`: what a file holds, like `osmium fileinfo -e`.
