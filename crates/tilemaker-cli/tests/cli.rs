//! Run the `tilemaker` binary end to end.

use std::path::{Path, PathBuf};
use std::process::Command;

use tilemaker_pbf::write::{PbfWriter, WriterOptions};
use tilemaker_pbf::{ElementType, HeaderBbox};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tilemaker"))
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tilemaker-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn e7(d: f64) -> i32 {
    (d * 1e7).round() as i32
}

fn fixture(dir: &Path) -> PathBuf {
    let path = dir.join("town.osm.pbf");
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
    for (id, lon, lat) in [
        (1, 0.001, 0.001),
        (2, 0.019, 0.001),
        (3, 0.019, 0.019),
        (4, 0.001, 0.019),
    ] {
        w.node(id, e7(lat), e7(lon), &[]).unwrap();
    }
    w.node(20, e7(0.01), e7(0.01), &[("amenity", "cafe"), ("name", "Café")])
        .unwrap();
    w.way(100, &[1, 2, 3], &[("highway", "primary")]).unwrap();
    w.way(101, &[1, 2, 3, 4, 1], &[]).unwrap();
    w.relation(
        200,
        &[(101, ElementType::Way, "outer")],
        &[("type", "multipolygon"), ("landuse", "meadow")],
    )
    .unwrap();
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{"layers":{"poi":{"minzoom":10,"maxzoom":14},"roads":{"minzoom":5,"maxzoom":14},"landuse":{"minzoom":5,"maxzoom":14}},
            "settings":{"minzoom":5,"maxzoom":14,"basezoom":14,"include_ids":false,"name":"T","version":"1","description":"","compress":"gzip"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("process.lua"),
        r#"function node_function() if Find("amenity") ~= "" then Layer("poi"); Attribute("name", Find("name")) end end
           function way_function()
             if Find("highway") ~= "" then Layer("roads", false) end
             if Find("landuse") ~= "" then Layer("landuse", true) end
           end"#,
    )
    .unwrap();
    path
}

#[test]
fn makes_tiles_in_every_format() {
    let d = dir("tiles");
    let pbf = fixture(&d);
    for out in ["t.mbtiles", "t.pmtiles", "tiles"] {
        let o = bin()
            .current_dir(&d)
            .args([pbf.to_str().unwrap(), out, "--quiet"])
            .output()
            .unwrap();
        assert!(o.status.success(), "{out}: {}", String::from_utf8_lossy(&o.stderr));
        assert!(d.join(out).exists(), "{out}");
    }
    assert!(d.join("tiles/14/8192/8191.pbf").exists());
    for f in ["t.mbtiles", "t.pmtiles"] {
        let o = bin().current_dir(&d).args(["inspect", f]).output().unwrap();
        assert!(o.status.success());
        let text = String::from_utf8(o.stdout).unwrap();
        assert!(text.contains("z14: 1 tiles"), "{f}: {text}");
        assert!(
            text.contains("poi=1") && text.contains("roads=1") && text.contains("landuse=1"),
            "{text}"
        );
    }
    let o = bin()
        .current_dir(&d)
        .args(["inspect", "--json", "t.mbtiles"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["14"]["features"]["poi"], 1);
}

#[test]
fn flags_work_like_tilemaker() {
    let d = dir("flags");
    let pbf = fixture(&d);
    let o = bin()
        .args([
            "--input",
            pbf.to_str().unwrap(),
            "--output",
            d.join("out.mbtiles").to_str().unwrap(),
            "--config",
            d.join("config.json").to_str().unwrap(),
            "--process",
            d.join("process.lua").to_str().unwrap(),
            "--threads",
            "2",
            "--bbox",
            "0,0,0.02,0.02",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("Wrote 10 tiles"), "{err}");
}

#[test]
fn exports_geojson_lines() {
    let d = dir("export");
    let pbf = fixture(&d);
    let o = bin()
        .args([
            "export",
            pbf.to_str().unwrap(),
            "-f",
            "n/amenity",
            "-f",
            "r/landuse",
            "--tag",
            "name",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let lines: Vec<serde_json::Value> = String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"], "node/20");
    assert_eq!(lines[0]["properties"]["name"], "Café");
    assert_eq!(lines[1]["geometry"]["type"], "MultiPolygon");
    let stats: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&o.stderr).trim()).unwrap();
    assert_eq!(stats["emitted"], 2);

    let out = d.join("out.geojsonseq");
    let o = bin()
        .args([
            "export",
            pbf.to_str().unwrap(),
            "--geometry-types",
            "linestring",
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(std::fs::read_to_string(out).unwrap().lines().count(), 1);
}

#[test]
fn export_enrichments_and_transforms() {
    let d = dir("enrich");
    let pbf = fixture(&d);
    let o = bin()
        .args([
            "export",
            pbf.to_str().unwrap(),
            "-f",
            "w/highway",
            "-f",
            "r/landuse",
            "--area",
            "--length",
            "--centroid",
            "--label-point",
            "--feature-bbox",
            "--member-of",
            "--clip",
            "0,0,0.01,0.02",
            "--precision",
            "3",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let lines: Vec<serde_json::Value> = String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    let road = &lines[0];
    assert_eq!(road["id"], "way/100");
    assert_eq!(road["bbox"], serde_json::json!([0.001, 0.001, 0.01, 0.001]));
    assert!(road["properties"]["@length"].as_f64().unwrap() > 1000.0);
    assert_eq!(road["properties"]["@memberOf"], serde_json::json!([]));
    let meadow = &lines[1];
    assert_eq!(meadow["geometry"]["type"], "MultiPolygon");
    assert!(meadow["properties"]["@area"].as_f64().unwrap() > 1e6);
    assert_eq!(meadow["properties"]["@labelPoint"].as_array().unwrap().len(), 2);
    // Unknown filters are errors.
    let o = bin()
        .args(["export", pbf.to_str().unwrap(), "--member-of", "=x"])
        .output()
        .unwrap();
    assert!(!o.status.success());
}

#[test]
fn reports_errors() {
    let d = dir("errors");
    let pbf = fixture(&d);
    let o = bin().arg(pbf.to_str().unwrap()).current_dir(&d).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("output"));
    std::fs::write(d.join("bad.json"), "{").unwrap();
    let o = bin()
        .current_dir(&d)
        .args([pbf.to_str().unwrap(), "x.mbtiles", "--config", "bad.json"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("bad.json"));
    let o = bin().args(["export", "/nonexistent.osm.pbf"]).output().unwrap();
    assert!(!o.status.success());
    let o = bin()
        .args(["export", pbf.to_str().unwrap(), "-f", "w/"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stderr).contains("invalid tag filter"));
}
