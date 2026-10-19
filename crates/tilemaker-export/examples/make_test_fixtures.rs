//! Write the small deterministic PBF files the Node.js tests use:
//! `cargo run -p tilemaker-export --example make_test_fixtures -- <dir>`.

use std::path::Path;

use tilemaker_pbf::write::{PbfWriter, WriterOptions};
use tilemaker_pbf::{ElementType, HeaderBbox, Info};

fn e7(d: f64) -> i32 {
    (d * 1e7).round() as i32
}

/// The same town as tests/synthetic.rs, with metadata on some elements.
fn town(path: &Path) {
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            bbox: Some(HeaderBbox {
                min_lon: -1.0,
                min_lat: -1.0,
                max_lon: 11.0,
                max_lat: 11.0,
            }),
            writing_program: Some("make_test_fixtures".into()),
            ..Default::default()
        },
    );
    let info = Info {
        version: 2,
        timestamp: 1_600_000_000,
        changeset: 9,
        uid: 3,
        visible: true,
    };
    for (id, lon, lat) in [
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
    ] {
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
    w.node(21, e7(10.0), e7(10.0), &[("shop", "bakery"), ("__proto__", "x")])
        .unwrap();
    w.node(14_274_176_831, e7(-0.5), e7(-0.5), &[("natural", "tree")])
        .unwrap();
    w.way_full(
        100,
        &[1, 2],
        &[],
        &[("highway", "residential"), ("name", "Main St")],
        Some((&info, "bob")),
    )
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
        &[
            (100, ElementType::Way, ""),
            (102, ElementType::Way, ""),
            (20, ElementType::Node, "stop"),
        ],
        &[("type", "route"), ("route", "bus")],
    )
    .unwrap();
    std::fs::write(path, w.finish().unwrap()).unwrap();
}

/// A grid of `n × n` tagged nodes, a way per row and a square multipolygon
/// per 10 × 10 cell: many blocks, for ordering and flow-control tests.
fn grid(path: &Path, n: i64) {
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            ..Default::default()
        },
    );
    let id = |r: i64, c: i64| r * n + c + 1;
    for r in 0..n {
        for c in 0..n {
            w.node(
                id(r, c),
                e7(r as f64 * 0.001),
                e7(c as f64 * 0.001),
                &[("ref", &format!("{r}:{c}"))],
            )
            .unwrap();
        }
    }
    for r in 0..n {
        let refs: Vec<i64> = (0..n).map(|c| id(r, c)).collect();
        w.way(1_000_000 + r, &refs, &[("highway", "service")]).unwrap();
    }
    let mut way_id = 2_000_000;
    let mut rel = Vec::new();
    for r in (0..n - 10).step_by(10) {
        for c in (0..n - 10).step_by(10) {
            let ring = [id(r, c), id(r, c + 10), id(r + 10, c + 10), id(r + 10, c), id(r, c)];
            w.way(way_id, &ring, &[]).unwrap();
            rel.push(way_id);
            way_id += 1;
        }
    }
    for (k, wid) in rel.iter().enumerate() {
        w.relation(
            3_000_000 + k as i64,
            &[(*wid, ElementType::Way, "outer")],
            &[("type", "multipolygon"), ("landuse", "grass")],
        )
        .unwrap();
    }
    std::fs::write(path, w.finish().unwrap()).unwrap();
}

/// A file whose ways carry their locations.
fn locations_on_ways(path: &Path) {
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
        &[1, 2, 3],
        &[(e7(1.0), e7(2.0)), (e7(3.0), e7(4.0)), (e7(5.0), e7(6.0))],
        &[("highway", "x")],
        None,
    )
    .unwrap();
    std::fs::write(path, w.finish().unwrap()).unwrap();
}

/// A way whose second node is missing from the file (as in extracts cut
/// without complete ways).
fn broken(path: &Path) {
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            ..Default::default()
        },
    );
    w.node(1, e7(1.0), e7(1.0), &[]).unwrap();
    w.node(3, e7(2.0), e7(2.0), &[]).unwrap();
    w.way(10, &[1, 2, 3], &[("highway", "track")]).unwrap();
    w.way(11, &[1, 3], &[("highway", "path")]).unwrap();
    std::fs::write(path, w.finish().unwrap()).unwrap();
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: make_test_fixtures <dir>");
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    town(&dir.join("town.osm.pbf"));
    grid(&dir.join("grid.osm.pbf"), 300);
    locations_on_ways(&dir.join("low.osm.pbf"));
    broken(&dir.join("broken.osm.pbf"));
    std::fs::write(dir.join("not-a-pbf.osm.pbf"), b"this is not a PBF file").unwrap();
}
