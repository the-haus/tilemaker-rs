//! Tiles from a small hand-built file, checked exactly.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilemaker_config::Config;
use tilemaker_core::{generate, write_tiles, Event, Flow, Progress, Tile, TileOptions};
use tilemaker_mvt::{decode, GeomType, Value};
use tilemaker_pbf::write::{PbfWriter, WriterOptions};
use tilemaker_pbf::{ElementType, HeaderBbox, PbfFile};

fn e7(d: f64) -> i32 {
    (d * 1e7).round() as i32
}

static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tilemaker-core-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!(
        "{}-{name}",
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ))
}

/// A tiny town around (0.01, 0.01): a café, a road, a park (closed way)
/// and a lake with an island (multipolygon), all within z14 tile 8192/8191.
fn town() -> PathBuf {
    let path = temp("town.osm.pbf");
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            bbox: Some(HeaderBbox {
                min_lon: 0.0,
                min_lat: 0.0,
                max_lon: 0.02,
                max_lat: 0.02,
            }),
            ..Default::default()
        },
    );
    let pts = [
        (1, 0.001, 0.001),
        (2, 0.019, 0.001),
        (3, 0.019, 0.019),
        (4, 0.001, 0.019),
        (5, 0.005, 0.005),
        (6, 0.008, 0.005),
        (7, 0.008, 0.008),
        (8, 0.005, 0.008),
        (9, 0.006, 0.006),
        (10, 0.007, 0.006),
        (11, 0.007, 0.007),
        (12, 0.006, 0.007),
    ];
    for (id, lon, lat) in pts {
        w.node(id, e7(lat), e7(lon), &[]).unwrap();
    }
    w.node(20, e7(0.01), e7(0.01), &[("amenity", "cafe"), ("name", "Café")])
        .unwrap();
    w.way(100, &[1, 2, 3], &[("highway", "primary"), ("name", "Main")])
        .unwrap();
    w.way(101, &[5, 6, 7, 8, 5], &[("leisure", "park")]).unwrap();
    w.way(102, &[1, 2, 3, 4, 1], &[]).unwrap();
    w.way(103, &[9, 10, 11, 12, 9], &[]).unwrap();
    w.relation(
        200,
        &[(102, ElementType::Way, "outer"), (103, ElementType::Way, "inner")],
        &[("type", "multipolygon"), ("natural", "water")],
    )
    .unwrap();
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    path
}

const CONFIG: &str = r#"{
  "layers": {
    "poi": { "minzoom": 12, "maxzoom": 14 },
    "roads": { "minzoom": 0, "maxzoom": 14, "simplify_below": 10, "simplify_level": 0.001 },
    "water": { "minzoom": 4, "maxzoom": 14 },
    "parks": { "minzoom": 10, "maxzoom": 14, "write_to": "water" }
  },
  "settings": { "minzoom": 0, "maxzoom": 14, "basezoom": 14, "include_ids": true,
                "name": "Town", "version": "1.0", "description": "test", "compress": "gzip" }
}"#;

const PROFILE: &str = r#"
node_keys = { "amenity" }
function node_function()
  Layer("poi")
  Attribute("name", Find("name"))
  AttributeInteger("rank", 1, 14)
end
function way_function()
  if Find("highway") ~= "" then
    Layer("roads", false)
    Attribute("class", Find("highway"))
    MinZoom(2)
  elseif Find("leisure") == "park" then
    Layer("parks", true)
    Attribute("class", "park")
  elseif Find("natural") == "water" then
    Layer("water", true)
    Attribute("class", "lake")
    AttributeNumeric("area", Area())
  end
end
"#;

fn run(opts: &TileOptions) -> (Vec<Tile>, tilemaker_core::TileStats, tilemaker_output::TilesetMetadata) {
    let file = PbfFile::open(town()).unwrap();
    let config = Config::from_json(CONFIG).unwrap();
    let mut tiles = Vec::new();
    let (stats, meta) = generate(&file, &config, PROFILE, "test.lua", opts, |e| {
        if let Event::Tile(t) = e {
            tiles.push(t);
        }
        Flow::Continue
    })
    .unwrap();
    (tiles, stats, meta)
}

fn layer_counts(t: &Tile) -> BTreeMap<String, usize> {
    decode(&t.data)
        .unwrap()
        .layers
        .into_iter()
        .map(|l| (l.name, l.features.len()))
        .collect()
}

#[test]
fn tiles_per_zoom_and_layers() {
    let (tiles, stats, _) = run(&TileOptions::default());
    // One tile per zoom from z3: the road's minzoom is 2, but at z2 the
    // 0.018° road is under one tile unit long, so that tile is empty.
    let zooms: Vec<u8> = tiles.iter().map(|t| t.z).collect();
    assert_eq!(zooms, (3..=14).collect::<Vec<_>>());
    assert_eq!(stats.tiles, 12);
    assert_eq!(stats.read.objects, 4);
    assert_eq!(stats.read.nodes_processed, 1);
    let at = |z: u8| layer_counts(tiles.iter().find(|t| t.z == z).unwrap());
    assert_eq!(at(3), BTreeMap::from([("roads".into(), 1)]));
    assert_eq!(at(4), BTreeMap::from([("roads".into(), 1), ("water".into(), 1)]));
    assert_eq!(at(10), BTreeMap::from([("roads".into(), 1), ("water".into(), 2)]));
    assert_eq!(
        at(14),
        BTreeMap::from([("poi".into(), 1), ("roads".into(), 1), ("water".into(), 2)])
    );
    // Layers come out in config order (write_to layers inside their target).
    let names: Vec<String> = decode(&tiles.last().unwrap().data)
        .unwrap()
        .layers
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, vec!["poi", "roads", "water"]);
    let t14 = tiles.last().unwrap();
    assert_eq!((t14.x, t14.y), (8192, 8191));
}

#[test]
fn features_are_drawn_correctly() {
    let (tiles, _, _) = run(&TileOptions::default());
    let t14 = decode(&tiles.last().unwrap().data).unwrap();
    let poi = t14.layers.iter().find(|l| l.name == "poi").unwrap();
    let f = &poi.features[0];
    assert_eq!(f.geom_type, GeomType::Point);
    assert_eq!(f.id, Some(20));
    let props = f.properties(poi);
    assert!(props.contains(&("name", &Value::String("Café".into()))));
    assert!(props.contains(&("rank", &Value::Int(1))));
    // (0.01, 0.01) in z14 tile 8192/8191: x = 0.01/360·2^14·4096 − 8192·4096.
    let g = f.decode_geometry();
    let (x, y) = g[0][0];
    assert!((x - 1864).abs() <= 1, "{x}");
    assert!((y - (4096 - 1864)).abs() <= 2, "{y}");

    let water = t14.layers.iter().find(|l| l.name == "water").unwrap();
    let lake = water
        .features
        .iter()
        .find(|f| {
            f.properties(water)
                .iter()
                .any(|(k, v)| *k == "class" && **v == Value::String("lake".into()))
        })
        .unwrap();
    assert_eq!(lake.geom_type, GeomType::Polygon);
    let rings = lake.decode_geometry();
    assert_eq!(rings.len(), 2, "exterior and island hole");
    let area = |r: &Vec<(i32, i32)>| -> f64 {
        r.windows(2)
            .map(|w| (w[0].0 as f64) * (w[1].1 as f64) - (w[1].0 as f64) * (w[0].1 as f64))
            .sum::<f64>()
            / 2.0
    };
    assert!(
        area(&rings[0]) > 0.0,
        "exterior clockwise in tile space (positive area)"
    );
    assert!(area(&rings[1]) < 0.0, "hole anticlockwise");
    // Area() of the lake: 0.018° squared minus 0.001° squared, in m².
    let a = lake
        .properties(water)
        .iter()
        .find(|(k, _)| *k == "area")
        .map(|(_, v)| (*v).clone())
        .unwrap();
    match a {
        Value::Float(m2) => assert!((m2 as f64 - 3_998_000.0).abs() < 20_000.0, "{m2}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn attribute_min_zoom_and_simplification() {
    let (tiles, _, _) = run(&TileOptions::default());
    let t13 = decode(&tiles.iter().find(|t| t.z == 13).unwrap().data).unwrap();
    let poi = t13.layers.iter().find(|l| l.name == "poi").unwrap();
    assert!(
        poi.features[0].properties(poi).iter().all(|(k, _)| *k != "rank"),
        "rank only from z14"
    );
    // At z5 the tolerance (0.001° · 2^4) exceeds the corner's offset from
    // the chord: the road is simplified to its end points.
    let t5 = decode(&tiles.iter().find(|t| t.z == 5).unwrap().data).unwrap();
    let road = &t5.layers.iter().find(|l| l.name == "roads").unwrap().features[0];
    assert_eq!(road.decode_geometry()[0].len(), 2);
    let t14 = decode(&tiles.last().unwrap().data).unwrap();
    let road = &t14.layers.iter().find(|l| l.name == "roads").unwrap().features[0];
    assert_eq!(road.decode_geometry()[0].len(), 3);
}

#[test]
fn metadata() {
    let (_, _, meta) = run(&TileOptions::default());
    assert_eq!(meta.name, "Town");
    assert_eq!((meta.minzoom, meta.maxzoom), (0, 14));
    assert_eq!(meta.bounds, Some([0.0, 0.0, 0.02, 0.02]));
    let layers: serde_json::Value = serde_json::from_str(&meta.vector_layers_json).unwrap();
    let ids: Vec<&str> = layers
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["poi", "roads", "water"]);
    assert_eq!(layers[0]["fields"]["rank"], "Number");
    assert_eq!(layers[2]["fields"]["class"], "String", "parks' fields land in water");
    assert_eq!(layers[2]["fields"]["area"], "Number");
}

#[test]
fn bbox_option_limits_tiles() {
    let (tiles, _, meta) = run(&TileOptions {
        bbox: Some([10.0, 10.0, 11.0, 11.0]),
        ..Default::default()
    });
    assert!(
        tiles.iter().all(|t| t.z < 7),
        "only low zooms reach (10, 10): {:?}",
        tiles.iter().map(|t| t.z).collect::<Vec<_>>()
    );
    assert_eq!(meta.bounds, Some([10.0, 10.0, 11.0, 11.0]));
}

#[test]
fn progress_and_stopping() {
    let file = PbfFile::open(town()).unwrap();
    let config = Config::from_json(CONFIG).unwrap();
    let mut phases = Vec::new();
    let mut tiles = 0;
    let (stats, _) = generate(
        &file,
        &config,
        PROFILE,
        "test.lua",
        &TileOptions::default(),
        |e| match e {
            Event::Progress(Progress::Read { phase, .. }) => {
                if phases.last() != Some(&phase) {
                    phases.push(phase);
                }
                Flow::Continue
            }
            Event::Tile(_) => {
                tiles += 1;
                if tiles == 3 {
                    Flow::Stop
                } else {
                    Flow::Continue
                }
            }
            _ => Flow::Continue,
        },
    )
    .unwrap();
    use tilemaker_core::Phase::*;
    // No way_keys in the profile: no ScanWays pass.
    assert_eq!(phases, vec![ScanRelations, Nodes, Ways, Relations]);
    assert!(stats.stopped);
    assert_eq!(tiles, 3);
}

#[test]
fn writes_every_output_format() {
    let file = PbfFile::open(town()).unwrap();
    let config = Config::from_json(CONFIG).unwrap();
    for name in ["town.mbtiles", "town.pmtiles", "town-dir"] {
        let path = temp(name);
        let is_dir = name == "town-dir";
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&path);
        let mut sink = tilemaker_output::open_sink(&path, tilemaker_output::Compression::Gzip, false).unwrap();
        let (stats, _) = write_tiles(
            &file,
            &config,
            PROFILE,
            "test.lua",
            &TileOptions::default(),
            sink.as_mut(),
            |_| Flow::Continue,
        )
        .unwrap();
        assert_eq!(stats.tiles, 12, "{name}");
        drop(sink);
        assert!(path.exists(), "{name}");
        if is_dir {
            assert!(path.join("14/8192/8191.pbf").exists());
        }
    }
    let pm = std::fs::read_dir(temp("x").parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with("town.pmtiles"))
        .unwrap();
    let r = tilemaker_output::pmtiles::reader::PmtilesReader::open(pm).unwrap();
    let t = r.get_tile_decompressed(14, 8192, 8191).unwrap().unwrap();
    assert_eq!(decode(&t).unwrap().layers.len(), 3);
}

#[test]
fn profile_errors_are_reported() {
    let file = PbfFile::open(town()).unwrap();
    let config = Config::from_json(CONFIG).unwrap();
    let err = generate(
        &file,
        &config,
        "function way_function() Layer('nope') end",
        "bad.lua",
        &TileOptions::default(),
        |_| Flow::Continue,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("nope") && err.contains("way"), "{err}");
    let err = generate(
        &file,
        &config,
        "syntax error here",
        "bad.lua",
        &TileOptions::default(),
        |_| Flow::Continue,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("bad.lua"), "{err}");
}

#[test]
fn deterministic_across_thread_counts() {
    let a = run(&TileOptions {
        threads: 1,
        ..Default::default()
    })
    .0;
    let b = run(&TileOptions {
        threads: 8,
        ..Default::default()
    })
    .0;
    assert_eq!(a, b);
}

/// Ten cafés with ranks: a layer limited to 3 features keeps the highest
/// z_order (descending when a limit is set), and identical points merge.
#[test]
fn feature_limit_z_order_and_multipoints() {
    let path = temp("cafes.osm.pbf");
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            ..Default::default()
        },
    );
    for i in 1..=10i64 {
        let rank = i.to_string();
        w.node(
            i,
            e7(0.001 * i as f64),
            e7(0.001 * i as f64),
            &[("amenity", "cafe"), ("rank", &rank)],
        )
        .unwrap();
    }
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    let config = Config::from_json(
        r#"{"layers":{"top":{"minzoom":14,"maxzoom":14,"feature_limit":3},"all":{"minzoom":14,"maxzoom":14}},
            "settings":{"minzoom":14,"maxzoom":14,"basezoom":14,"name":"x","version":"1","description":"","compress":"none"}}"#,
    )
    .unwrap();
    let profile = r#"
      function node_function()
        local r = tonumber(Find("rank"))
        Layer("top"); Attribute("rank", Find("rank")); ZOrder(r)
        Layer("all"); Attribute("kind", "cafe")
      end"#;
    let file = PbfFile::open(&path).unwrap();
    let mut tiles = Vec::new();
    generate(&file, &config, profile, "p.lua", &TileOptions::default(), |e| {
        if let Event::Tile(t) = e {
            tiles.push(t);
        }
        Flow::Continue
    })
    .unwrap();
    assert_eq!(tiles.len(), 1);
    let t = decode(&tiles[0].data).unwrap();
    let top = t.layers.iter().find(|l| l.name == "top").unwrap();
    let ranks: Vec<String> = top
        .features
        .iter()
        .map(|f| match f.properties(top)[0].1 {
            Value::String(s) => s.clone(),
            v => panic!("{v:?}"),
        })
        .collect();
    assert_eq!(ranks, vec!["10", "9", "8"]);
    // Identical attributes: the ten cafés become one multipoint.
    let all = t.layers.iter().find(|l| l.name == "all").unwrap();
    assert_eq!(all.features.len(), 1);
    assert_eq!(all.features[0].decode_geometry()[0].len(), 10);
}
