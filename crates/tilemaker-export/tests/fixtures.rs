//! Compare with `osmium export` on real extracts (see fixtures/README.md).
//! Skipped when the extracts are absent.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilemaker_export::*;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

#[derive(Debug)]
struct Shape {
    polygons: usize,
    rings: usize,
    points: usize,
    area: f64,
}

fn shape(g: &Geometry) -> Shape {
    let mut area = 0.0;
    let mut start = 0usize;
    let mut ring_start = 0usize;
    for &pend in &g.polygons {
        for r in ring_start..pend as usize {
            let end = g.parts[r] as usize;
            let c = &g.coords[start * 2..end * 2];
            let n = c.len() / 2;
            let a: f64 = (0..n - 1)
                .map(|i| c[2 * i] * c[2 * i + 3] - c[2 * i + 2] * c[2 * i + 1])
                .sum::<f64>()
                / 2.0;
            area += a; // holes are clockwise, so this subtracts them
            start = end;
        }
        ring_start = pend as usize;
    }
    Shape {
        polygons: g.polygons.len(),
        rings: g.parts.len(),
        points: g.point_count(),
        area,
    }
}

fn reference(name: &str) -> BTreeMap<String, serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join("ref").join(name)).unwrap()).unwrap()
}

fn compare(pbf: &str, filter: &str, refname: &str) {
    let path = fixtures().join(pbf);
    if !path.exists() {
        eprintln!("skipping: {} not found", path.display());
        return;
    }
    let file = PbfFile::open(&path).unwrap();
    let opts = FeatureOptions {
        common: CommonOptions {
            filters: FilterSet::parse(&[filter]).unwrap(),
            ..Default::default()
        },
        geometry_types: GeometryTypes {
            points: false,
            lines: false,
            polygons: true,
        },
        ..Default::default()
    };
    let mut ours = BTreeMap::new();
    export_features(&file, &opts, |e| {
        if let Event::Batch(b) = e {
            for f in b {
                if f.kind == ElementType::Relation {
                    ours.insert(f.id.to_string(), shape(&f.geometry));
                }
            }
        }
        Flow::Continue
    })
    .unwrap();
    let want = reference(refname);
    assert_eq!(
        ours.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>(),
        "the same relations assemble"
    );
    for (id, w) in &want {
        let o = &ours[id];
        assert_eq!(
            o.polygons as u64,
            w["polygons"].as_u64().unwrap(),
            "relation {id} polygons"
        );
        assert_eq!(o.rings as u64, w["rings"].as_u64().unwrap(), "relation {id} rings");
        assert_eq!(o.points as u64, w["points"].as_u64().unwrap(), "relation {id} points");
        let wa = w["area"].as_f64().unwrap();
        assert!(
            (o.area - wa).abs() <= wa.abs() * 1e-9 + 1e-12,
            "relation {id} area {} vs {wa}",
            o.area
        );
    }
}

#[test]
fn monaco_areas_match_osmium() {
    compare("monaco.osm.pbf", "r/type=multipolygon,boundary", "monaco-areas.json");
}

#[test]
fn liechtenstein_admin_areas_match_osmium() {
    compare(
        "liechtenstein.osm.pbf",
        "r/admin_level=2,4,6,8",
        "liechtenstein-admin.json",
    );
}

#[test]
fn monaco_points_match_osmium_tags_filter() {
    let path = fixtures().join("monaco.osm.pbf");
    if !path.exists() {
        return;
    }
    let file = PbfFile::open(&path).unwrap();
    let opts = FeatureOptions {
        common: CommonOptions {
            filters: FilterSet::parse(&["n/amenity"]).unwrap(),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut n = 0;
    export_features(&file, &opts, |e| {
        if let Event::Batch(b) = e {
            n += b.len();
        }
        Flow::Continue
    })
    .unwrap();
    // `osmium tags-filter monaco.osm.pbf n/amenity -R | wc -l`
    assert_eq!(n, 1_311);
}
