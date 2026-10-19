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
the [engine docs](src/engine.rs) for the passes.
