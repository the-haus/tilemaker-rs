//! Exports of small hand-built files, checked exactly.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use tilemaker_export::*;
use tilemaker_pbf::write::{PbfWriter, WriterOptions};
use tilemaker_pbf::Info;

static N: AtomicUsize = AtomicUsize::new(0);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tilemaker-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{}-{name}", N.fetch_add(1, Ordering::SeqCst)))
}

/// Degrees to OSM fixed point.
fn e7(d: f64) -> i32 {
    (d * 1e7).round() as i32
}

/// A small town:
/// * nodes 1–4: a square (0,0)-(1,1); 5–8 a hole (0.4..0.6); 9–12 a second
///   square (2,0)-(3,1); 20: a café; 21: a shop outside everything (10,10)
/// * way 100: highway line 1→2; way 101: building, closed square 1-2-3-4-1;
///   way 102: closed highway (a roundabout) 9-10-11-12-9; 103/104: the
///   outer ring of relation 200 split in two; 105: the hole
/// * relation 200: multipolygon (103+104 outer, 105 inner), landuse
/// * relation 201: boundary with a missing way (999)
/// * relation 202: route over ways 100 and 102 (not an area)
fn town(opts: WriterOptions) -> PathBuf {
    let path = temp("town.osm.pbf");
    let mut w = PbfWriter::with_options(Vec::new(), opts);
    let info = Info {
        version: 2,
        timestamp: 1_600_000_000,
        changeset: 9,
        uid: 3,
        visible: true,
    };
    let pts = [
        (1, 0.0, 0.0),
        (2, 1.0, 0.0),
        (3, 1.0, 1.0),
        (4, 0.0, 1.0),
        (5, 0.4, 0.4),
        (6, 0.6, 0.4),
        (7, 0.6, 0.6),
        (8, 0.4, 0.6),
        (9, 2.0, 0.0),
        (10, 3.0, 0.0),
        (11, 3.0, 1.0),
        (12, 2.0, 1.0),
    ];
    for (id, lon, lat) in pts {
        w.node(id, e7(lat), e7(lon), &[]).unwrap();
    }
    w.node_with_info(
        20,
        e7(0.5),
        e7(0.5),
        &[("amenity", "cafe"), ("name", "Café")],
        Some((&info, "alice")),
    )
    .unwrap();
    w.node(21, e7(10.0), e7(10.0), &[("shop", "bakery")]).unwrap();
    w.way(100, &[1, 2], &[("highway", "residential"), ("name", "Main St")])
        .unwrap();
    w.way(101, &[1, 2, 3, 4, 1], &[("building", "yes")]).unwrap();
    w.way(
        102,
        &[9, 10, 11, 12, 9],
        &[("highway", "primary"), ("junction", "roundabout")],
    )
    .unwrap();
    w.way(103, &[1, 2, 3], &[]).unwrap();
    w.way(104, &[1, 4, 3], &[]).unwrap();
    w.way(105, &[5, 6, 7, 8, 5], &[]).unwrap();
    w.relation(
        200,
        &[
            (103, ElementType::Way, "outer"),
            (104, ElementType::Way, "outer"),
            (105, ElementType::Way, "inner"),
        ],
        &[("type", "multipolygon"), ("landuse", "meadow")],
    )
    .unwrap();
    w.relation(
        201,
        &[(101, ElementType::Way, "outer"), (999, ElementType::Way, "outer")],
        &[
            ("type", "boundary"),
            ("boundary", "administrative"),
            ("admin_level", "8"),
        ],
    )
    .unwrap();
    w.relation(
        202,
        &[(100, ElementType::Way, ""), (102, ElementType::Way, "")],
        &[("type", "route"), ("route", "bus")],
    )
    .unwrap();
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    path
}

fn sorted() -> WriterOptions {
    WriterOptions {
        sorted: true,
        ..Default::default()
    }
}

fn features(path: &PathBuf, opts: &FeatureOptions) -> (Vec<Feature>, ExportStats) {
    let file = PbfFile::open(path).unwrap();
    let mut out = Vec::new();
    let stats = export_features(&file, opts, |e| {
        if let Event::Batch(b) = e {
            out.extend(b);
        }
        Flow::Continue
    })
    .unwrap();
    (out, stats)
}

fn elements(path: &PathBuf, opts: &ElementOptions) -> (Vec<Element>, ExportStats) {
    let file = PbfFile::open(path).unwrap();
    let mut out = Vec::new();
    let stats = export_elements(&file, opts, |e| {
        if let Event::Batch(b) = e {
            out.extend(b);
        }
        Flow::Continue
    })
    .unwrap();
    (out, stats)
}

fn key(f: &Feature) -> (ElementType, i64, GeometryType) {
    (f.kind, f.id, f.geometry.kind)
}

fn filters(exprs: &[&str]) -> FilterSet {
    FilterSet::parse(exprs).unwrap()
}

fn signed_area(c: &[f64]) -> f64 {
    let n = c.len() / 2;
    (0..n - 1)
        .map(|i| c[2 * i] * c[2 * i + 3] - c[2 * i + 2] * c[2 * i + 1])
        .sum::<f64>()
        / 2.0
}

#[test]
fn everything_by_default() {
    for opts in [sorted(), WriterOptions::default()] {
        let path = town(opts);
        let (fs, stats) = features(&path, &FeatureOptions::default());
        let keys: Vec<_> = fs.iter().map(key).collect();
        assert_eq!(
            keys,
            vec![
                (ElementType::Node, 20, GeometryType::Point),
                (ElementType::Node, 21, GeometryType::Point),
                (ElementType::Way, 100, GeometryType::LineString),
                (ElementType::Way, 101, GeometryType::MultiPolygon),
                (ElementType::Way, 102, GeometryType::LineString),
                (ElementType::Relation, 200, GeometryType::MultiPolygon),
            ]
        );
        assert_eq!(stats.relations_missing_ways, 1, "boundary 201 lacks way 999");
        assert_eq!(stats.emitted, 6);
        assert_eq!(
            (stats.points, stats.lines, stats.polygons, stats.multi_lines),
            (2, 2, 2, 0)
        );
        assert!(!stats.stopped);
    }
}

#[test]
fn multipolygon_assembly() {
    let path = town(sorted());
    let opts = FeatureOptions {
        common: CommonOptions {
            filters: filters(&["r/landuse"]),
            ..Default::default()
        },
        ..Default::default()
    };
    let (fs, stats) = features(&path, &opts);
    assert_eq!(fs.len(), 1);
    let g = &fs[0].geometry;
    assert_eq!(g.kind, GeometryType::MultiPolygon);
    assert_eq!(g.polygons, vec![2]);
    assert_eq!(g.parts, vec![5, 10]);
    let outer = &g.coords[..10];
    let hole = &g.coords[10..];
    assert!(signed_area(outer) > 0.0, "exterior counter-clockwise");
    assert!(signed_area(hole) < 0.0, "hole clockwise");
    assert!((signed_area(outer) - 1.0).abs() < 1e-9);
    assert!((signed_area(hole) + 0.04).abs() < 1e-9);
    // Only the way and node passes needed for relation 200 stored anything.
    assert_eq!(stats.stored_ways, 3);
    assert_eq!(stats.stored_nodes, 8);
    let gj = fs[0].to_geojson();
    assert_eq!(gj["id"], "relation/200");
    assert_eq!(gj["properties"]["landuse"], "meadow");
    assert_eq!(gj["geometry"]["coordinates"][0][1].as_array().unwrap().len(), 5);
}

#[test]
fn closed_way_rules() {
    let path = town(sorted());
    let mk = |cw| FeatureOptions {
        common: CommonOptions {
            filters: filters(&["w/building", "w/junction"]),
            ..Default::default()
        },
        closed_ways: cw,
        ..Default::default()
    };
    let kinds = |cw| {
        features(&path, &mk(cw))
            .0
            .iter()
            .map(|f| (f.id, f.geometry.kind))
            .collect::<Vec<_>>()
    };
    use GeometryType::*;
    assert_eq!(kinds(ClosedWays::Auto), vec![(101, MultiPolygon), (102, LineString)]);
    assert_eq!(
        kinds(ClosedWays::Polygon),
        vec![(101, MultiPolygon), (102, MultiPolygon)]
    );
    assert_eq!(
        kinds(ClosedWays::LineString),
        vec![(101, LineString), (102, LineString)]
    );
    assert_eq!(
        kinds(ClosedWays::Both),
        vec![
            (101, MultiPolygon),
            (101, LineString),
            (102, MultiPolygon),
            (102, LineString)
        ]
    );
    // Way polygons are counter-clockwise, whatever the way's direction.
    for f in features(&path, &mk(ClosedWays::Polygon)).0 {
        assert!(signed_area(&f.geometry.coords) > 0.0);
    }
}

#[test]
fn area_filter_prefix() {
    let path = town(sorted());
    let opts = FeatureOptions {
        common: CommonOptions {
            filters: filters(&["a/building", "a/highway"]),
            ..Default::default()
        },
        ..Default::default()
    };
    let (fs, _) = features(&path, &opts);
    // The roundabout is not an area under the default rules; the line way 100 isn't closed.
    assert_eq!(
        fs.iter().map(key).collect::<Vec<_>>(),
        vec![(ElementType::Way, 101, GeometryType::MultiPolygon)]
    );
}

#[test]
fn geometry_type_selection() {
    let path = town(sorted());
    let only = |points, lines, polygons| FeatureOptions {
        geometry_types: GeometryTypes {
            points,
            lines,
            polygons,
        },
        ..Default::default()
    };
    let (fs, stats) = features(&path, &only(true, false, false));
    assert!(fs.iter().all(|f| f.geometry.kind == GeometryType::Point));
    assert_eq!(fs.len(), 2);
    assert_eq!(stats.stored_nodes, 0, "points alone need no store");
    let (fs, _) = features(&path, &only(false, false, true));
    assert_eq!(fs.iter().map(|f| f.id).collect::<Vec<_>>(), vec![101, 200]);
    let (fs, _) = features(&path, &only(false, true, false));
    assert_eq!(fs.iter().map(|f| f.id).collect::<Vec<_>>(), vec![100, 102]);
}

#[test]
fn relation_lines() {
    let path = town(sorted());
    let opts = FeatureOptions {
        common: CommonOptions {
            filters: filters(&["r/route"]),
            ..Default::default()
        },
        relation_lines: true,
        ..Default::default()
    };
    let (fs, stats) = features(&path, &opts);
    assert_eq!(fs.len(), 1);
    let g = &fs[0].geometry;
    assert_eq!(g.kind, GeometryType::MultiLineString);
    assert_eq!(g.parts, vec![2, 7], "two separate runs: the street and the roundabout");
    assert_eq!(stats.multi_lines, 1);
}

#[test]
fn bbox_tags_and_metadata() {
    let path = town(sorted());
    let opts = FeatureOptions {
        common: CommonOptions {
            filters: filters(&["n/amenity", "n/shop"]),
            bbox: Some([-1.0, -1.0, 5.0, 5.0]),
            tags: TagSelection::keys(&["name"]),
            metadata: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let (fs, _) = features(&path, &opts);
    assert_eq!(fs.len(), 1, "the bakery at (10, 10) is outside");
    assert_eq!(fs[0].tags, vec![("name".to_string(), "Café".to_string())]);
    let m = fs[0].meta.as_ref().unwrap();
    assert_eq!(
        (m.version, m.timestamp, m.changeset, m.uid, m.user.as_str()),
        (2, 1_600_000_000, 9, 3, "alice")
    );
    assert_eq!(fs[0].geometry.coords, vec![0.5, 0.5]);
}

#[test]
fn stopping_early() {
    let path = town(sorted());
    let file = PbfFile::open(&path).unwrap();
    let mut got = 0;
    let stats = export_features(&file, &FeatureOptions::default(), |e| {
        if let Event::Batch(b) = e {
            got += b.len();
            return Flow::Stop;
        }
        Flow::Continue
    })
    .unwrap();
    assert!(stats.stopped);
    assert_eq!(got, 2, "only the first block's points");
}

#[test]
fn progress_events() {
    let path = town(sorted());
    let file = PbfFile::open(&path).unwrap();
    let mut phases = Vec::new();
    export_features(&file, &FeatureOptions::default(), |e| {
        if let Event::Progress(p) = e {
            if phases.last() != Some(&p.phase) {
                phases.push(p.phase);
            }
            assert!(p.blocks_done <= p.blocks_total);
        }
        Flow::Continue
    })
    .unwrap();
    assert_eq!(
        phases,
        vec![
            Phase::ScanRelations,
            Phase::ScanWays,
            Phase::Nodes,
            Phase::Ways,
            Phase::Relations
        ]
    );
}

#[test]
fn dense_file_node_storage() {
    let path = town(sorted());
    let store = temp("nodes.bin");
    let opts = FeatureOptions {
        common: CommonOptions {
            node_storage: NodeStorage::DenseFile(store.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let (a, _) = features(&path, &opts);
    let (b, _) = features(&path, &FeatureOptions::default());
    assert_eq!(a, b);
    assert!(!store.exists(), "the store file is removed");
}

#[test]
fn missing_nodes_are_counted() {
    let path = temp("missing.osm.pbf");
    let mut w = PbfWriter::with_options(Vec::new(), sorted());
    w.node(1, 0, 0, &[]).unwrap();
    w.way(10, &[1, 2], &[("highway", "x")]).unwrap();
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    let (fs, stats) = features(&path, &FeatureOptions::default());
    assert!(fs.is_empty());
    assert_eq!(stats.ways_missing_nodes, 1);
}

#[test]
fn locations_on_ways() {
    let path = temp("low.osm.pbf");
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            locations_on_ways: true,
            ..Default::default()
        },
    );
    w.way_full(
        10,
        &[1, 2],
        &[(e7(1.0), e7(2.0)), (e7(3.0), e7(4.0))],
        &[("highway", "x")],
        None,
    )
    .unwrap();
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    let (fs, stats) = features(&path, &FeatureOptions::default());
    assert_eq!(fs[0].geometry.coords, vec![2.0, 1.0, 4.0, 3.0]);
    assert_eq!(stats.stored_nodes, 0);
}

#[test]
fn elements_single_pass() {
    let path = town(sorted());
    let opts = ElementOptions {
        common: CommonOptions {
            filters: filters(&["w/highway", "r/type=route"]),
            ..Default::default()
        },
        ..Default::default()
    };
    let (es, stats) = elements(&path, &opts);
    let ids: Vec<_> = es.iter().map(|e| (e.kind, e.id)).collect();
    assert_eq!(
        ids,
        vec![
            (ElementType::Way, 100),
            (ElementType::Way, 102),
            (ElementType::Relation, 202)
        ]
    );
    assert_eq!(es[0].refs, vec![1, 2]);
    assert!(es[0].locations.is_none());
    assert_eq!(es[2].members.len(), 2);
    assert_eq!(es[2].members[1].id, 102);
    assert_eq!(stats.stored_nodes, 0);
}

#[test]
fn elements_with_way_locations() {
    let path = town(sorted());
    let opts = ElementOptions {
        common: CommonOptions {
            filters: filters(&["w/highway=residential", "n/amenity"]),
            ..Default::default()
        },
        way_locations: true,
        ..Default::default()
    };
    let (es, stats) = elements(&path, &opts);
    assert_eq!(es.len(), 2);
    assert_eq!(es[0].location, Some([0.5, 0.5]));
    assert_eq!(es[1].locations.as_deref(), Some(&[0.0, 0.0, 1.0, 0.0][..]));
    assert_eq!(stats.stored_nodes, 2);
}

#[test]
fn elements_untagged_and_types() {
    let path = town(sorted());
    let mut f = FilterSet::default();
    f.include_untagged = true;
    let opts = ElementOptions {
        common: CommonOptions {
            filters: f,
            ..Default::default()
        },
        ways: false,
        relations: false,
        ..Default::default()
    };
    let (es, _) = elements(&path, &opts);
    assert_eq!(es.len(), 14);
    assert!(es.iter().all(|e| e.kind == ElementType::Node));
}

#[test]
fn many_blocks_keep_order() {
    // 30,000 tagged nodes and 3,000 ways across several blocks.
    let path = temp("big.osm.pbf");
    let mut w = PbfWriter::with_options(Vec::new(), sorted());
    for i in 1..=30_000i64 {
        w.node(i, (i as i32) * 100, (i as i32) * 100, &[("n", "y")]).unwrap();
    }
    for i in 1..=3_000i64 {
        w.way(i, &[i, i + 1, i + 2], &[("highway", "x")]).unwrap();
    }
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    let opts = FeatureOptions {
        common: CommonOptions {
            threads: 4,
            ..Default::default()
        },
        ..Default::default()
    };
    let (fs, stats) = features(&path, &opts);
    assert_eq!(fs.len(), 33_000);
    assert!(fs[..30_000].windows(2).all(|w| w[0].id < w[1].id));
    assert!(fs[30_000..].windows(2).all(|w| w[0].id < w[1].id));
    assert_eq!(stats.stored_nodes, 3_002);
}

#[test]
fn ways_pass_skips_blocks_without_needed_ways() {
    // 3 blocks of ways; the only relation uses a way in the middle block.
    let path = temp("skip.osm.pbf");
    let mut w = PbfWriter::with_options(Vec::new(), sorted());
    for (i, lon, lat) in [(1, 0.0, 0.0), (2, 1.0, 0.0), (3, 1.0, 1.0), (4, 0.0, 1.0)] {
        w.node(i, e7(lat), e7(lon), &[]).unwrap();
    }
    for i in 1..=24_000i64 {
        w.way(i, &[1, 2, 3, 4, 1], &[]).unwrap();
    }
    w.relation(
        1,
        &[(12_000, ElementType::Way, "outer")],
        &[("type", "multipolygon"), ("landuse", "x")],
    )
    .unwrap();
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    let file = PbfFile::open(&path).unwrap();
    let opts = FeatureOptions {
        common: CommonOptions {
            filters: filters(&["r/landuse"]),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut totals = std::collections::HashMap::new();
    let mut features = 0;
    export_features(&file, &opts, |e| {
        match e {
            Event::Progress(p) => {
                totals.insert(p.phase, p.blocks_total);
            }
            Event::Batch(b) => features += b.len(),
        }
        Flow::Continue
    })
    .unwrap();
    assert_eq!(features, 1);
    assert_eq!(totals[&Phase::ScanWays], 3);
    assert_eq!(totals[&Phase::Ways], 1);
}
