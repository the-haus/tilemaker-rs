//! Compare with the C++ tilemaker on real extracts (OpenMapTiles profile).
//! The references in fixtures/ref were made with `tilemaker inspect --json`
//! on the C++ build's output. Skipped when the extracts are absent.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilemaker_config::Config;
use tilemaker_core::{generate, Event, Flow, TileOptions};
use tilemaker_pbf::PbfFile;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn compare(name: &str) {
    let pbf = root().join("fixtures").join(format!("{name}.osm.pbf"));
    if !pbf.exists() {
        eprintln!("skipping: {} not found", pbf.display());
        return;
    }
    let reference: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root()
                .join("fixtures/ref")
                .join(format!("{name}-openmaptiles-cpp.json")),
        )
        .unwrap(),
    )
    .unwrap();
    let config = Config::from_path(root().join("resources/config-openmaptiles.json")).unwrap();
    let profile = std::fs::read_to_string(root().join("resources/process-openmaptiles.lua")).unwrap();
    let file = PbfFile::open(&pbf).unwrap();
    let mut ours: BTreeMap<u8, BTreeMap<String, u64>> = BTreeMap::new();
    generate(
        &file,
        &config,
        &profile,
        "process-openmaptiles.lua",
        &TileOptions::default(),
        |e| {
            if let Event::Tile(t) = e {
                let entry = ours.entry(t.z).or_default();
                for l in tilemaker_mvt::decode(&t.data).unwrap().layers {
                    *entry.entry(l.name).or_default() += l.features.len() as u64;
                }
            }
            Flow::Continue
        },
    )
    .unwrap();
    let mut total_ref = 0u64;
    let mut total_diff = 0u64;
    for (z, v) in reference.as_object().unwrap() {
        let z: u8 = z.parse().unwrap();
        for (layer, n) in v["features"].as_object().unwrap() {
            let want = n.as_u64().unwrap();
            let got = ours.get(&z).and_then(|m| m.get(layer)).copied().unwrap_or(0);
            let diff = want.abs_diff(got);
            total_ref += want;
            total_diff += diff;
            // Within 1% (or 3 features) per zoom and layer.
            assert!(diff <= 3.max(want / 100), "{name} z{z} {layer}: C++ {want}, ours {got}");
        }
    }
    // And within 0.5% overall.
    assert!(
        total_diff * 200 <= total_ref,
        "{name}: {total_diff} of {total_ref} features differ"
    );
}

#[test]
fn monaco_matches_cpp_tilemaker() {
    compare("monaco");
}

#[test]
fn liechtenstein_matches_cpp_tilemaker() {
    compare("liechtenstein");
}
