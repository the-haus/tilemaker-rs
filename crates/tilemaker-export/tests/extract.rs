//! Extracts of small hand-built files, checked against what osmium
//! (`tags-filter`, `extract --strategy complete_ways`) makes of them.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use tilemaker_export::*;
use tilemaker_pbf::write::{PbfWriter, WriterOptions};
use tilemaker_pbf::{DecodeOptions, Info};

static N: AtomicUsize = AtomicUsize::new(0);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tilemaker-extract-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{}-{name}", N.fetch_add(1, Ordering::SeqCst)))
}

fn e7(d: f64) -> i32 {
    (d * 1e7).round() as i32
}

/// The routes file (osmium's output for it noted per test):
///
/// ```text
/// n1 (0,0) n2 (1,0) n3 (5,5) n4 (6,5) n5 (7,7) stop n6 (8,8) cafe n7 (.5,.5) n8 (9,9)
/// w10 n1,n2 highway   w11 n3,n4   w12 n7,n8 building
/// r100 route_master: w10, r101   r101 route: w11, n5, r102   r102 site: n6
/// r103 super: r100
/// ```
fn routes() -> PathBuf {
    let path = temp("routes.osm.pbf");
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            ..Default::default()
        },
    );
    let info = Info {
        version: 3,
        timestamp: 1_600_000_000,
        changeset: 5,
        uid: 9,
        visible: true,
    };
    for (id, x, y, tags) in [
        (1, 0.0, 0.0, vec![]),
        (2, 1.0, 0.0, vec![]),
        (3, 5.0, 5.0, vec![]),
        (4, 6.0, 5.0, vec![]),
        (5, 7.0, 7.0, vec![("public_transport", "stop")]),
        (6, 8.0, 8.0, vec![("amenity", "cafe")]),
        (7, 0.5, 0.5, vec![]),
        (8, 9.0, 9.0, vec![]),
    ] {
        w.node_with_info(id, e7(y), e7(x), &tags, Some((&info, "ann"))).unwrap();
    }
    w.way(10, &[1, 2], &[("highway", "primary")]).unwrap();
    w.way(11, &[3, 4], &[]).unwrap();
    w.way(12, &[7, 8], &[("building", "yes")]).unwrap();
    use tilemaker_pbf::ElementType::*;
    w.relation(
        100,
        &[(10, Way, "outer"), (101, Relation, "sub")],
        &[("type", "route_master")],
    )
    .unwrap();
    w.relation(
        101,
        &[(11, Way, ""), (5, Node, "stop"), (102, Relation, "x")],
        &[("type", "route")],
    )
    .unwrap();
    w.relation(102, &[(6, Node, "")], &[("type", "site")]).unwrap();
    w.relation(103, &[(100, Relation, "")], &[("type", "super")]).unwrap();
    std::fs::write(&path, w.finish().unwrap()).unwrap();
    path
}

/// Node, way and relation ids.
type Ids = (Vec<i64>, Vec<i64>, Vec<i64>);

/// What an extract wrote: ids per type.
fn ids(path: &PathBuf) -> Ids {
    let file = PbfFile::open(path).unwrap();
    let (mut n, mut w, mut r) = (Vec::new(), Vec::new(), Vec::new());
    for b in file.blocks(0..file.blobs().len(), DecodeOptions::ALL, 1) {
        let (_, b) = b.unwrap();
        n.extend(b.nodes().map(|x| x.id()));
        w.extend(b.ways().map(|x| x.id()));
        r.extend(b.relations().map(|x| x.id()));
    }
    (n, w, r)
}

fn run(input: &PathBuf, opts: ExtractOptions) -> (Ids, ExtractStats) {
    let out = temp("out.osm.pbf");
    let file = PbfFile::open(input).unwrap();
    let f = std::fs::File::create(&out).unwrap();
    let (_, stats) = extract(&file, &opts, f, |_| Flow::Continue).unwrap();
    (ids(&out), stats)
}

fn filtered(exprs: &[&str]) -> ExtractOptions {
    ExtractOptions {
        filters: Some(FilterSet::parse(exprs).unwrap()),
        ..Default::default()
    }
}

#[test]
fn tags_filter_adds_members_recursively() {
    let p = routes();
    // osmium tags-filter r/type=route_master
    let (got, stats) = run(&p, filtered(&["r/type=route_master"]));
    assert_eq!(got, (vec![1, 2, 3, 4, 5, 6], vec![10, 11], vec![100, 101, 102]));
    assert_eq!((stats.nodes, stats.ways, stats.relations), (6, 2, 3));
    // osmium tags-filter w/highway
    assert_eq!(run(&p, filtered(&["w/highway"])).0, (vec![1, 2], vec![10], vec![]));
    // osmium tags-filter -R w/highway
    let opts = ExtractOptions {
        omit_referenced: true,
        ..filtered(&["w/highway"])
    };
    assert_eq!(run(&p, opts).0, (vec![], vec![10], vec![]));
    // Nodes match on their own tags.
    assert_eq!(run(&p, filtered(&["n/amenity"])).0, (vec![6], vec![], vec![]));
}

#[test]
fn regions_are_complete_ways_with_parent_relations() {
    let p = routes();
    let region = |b: [f64; 4]| ExtractOptions {
        region: Some(Region::Bbox(b)),
        ..Default::default()
    };
    // osmium extract -b 0,0,1,1: n1, n2, n7 inside; w10 and w12 touch it
    // (n8 completes w12); r100 has w10; r103 is r100's parent.
    assert_eq!(
        run(&p, region([0.0, 0.0, 1.0, 1.0])).0,
        (vec![1, 2, 7, 8], vec![10, 12], vec![100, 103])
    );
    // osmium extract -b 0.4,0.4,0.6,0.6
    assert_eq!(run(&p, region([0.4, 0.4, 0.6, 0.6])).0, (vec![7, 8], vec![12], vec![]));
    // The same as a polygon (a square with a hole around nothing).
    let square = serde_json::json!({"type": "Polygon", "coordinates": [
        [[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6], [0.4, 0.4]],
        [[0.41, 0.41], [0.42, 0.41], [0.42, 0.42], [0.41, 0.41]]
    ]});
    let opts = ExtractOptions {
        region: Some(Region::from_geojson(&square).unwrap()),
        ..Default::default()
    };
    assert_eq!(run(&p, opts).0, (vec![7, 8], vec![12], vec![]));
    // A hole excludes its inside.
    let holed = serde_json::json!({"type": "MultiPolygon", "coordinates": [[
        [[-1, -1], [2, -1], [2, 2], [-1, 2], [-1, -1]],
        [[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6], [0.4, 0.4]]
    ]]});
    let opts = ExtractOptions {
        region: Some(Region::from_geojson(&holed).unwrap()),
        ..Default::default()
    };
    assert_eq!(run(&p, opts).0, (vec![1, 2], vec![10], vec![100, 103]));
    // A region and filters: osmium extract, then osmium tags-filter.
    let opts = ExtractOptions {
        region: Some(Region::Bbox([0.0, 0.0, 1.0, 1.0])),
        ..filtered(&["r/type=route_master"])
    };
    // r101 is not in the extract, so not completed.
    assert_eq!(run(&p, opts).0, (vec![1, 2], vec![10], vec![100]));
}

#[test]
fn copies_tags_and_metadata() {
    let p = routes();
    let out = temp("copy.osm.pbf");
    let file = PbfFile::open(&p).unwrap();
    let (_, stats) = extract(
        &file,
        &ExtractOptions::default(),
        std::fs::File::create(&out).unwrap(),
        |_| Flow::Continue,
    )
    .unwrap();
    assert_eq!((stats.nodes, stats.ways, stats.relations), (8, 3, 4));
    let copy = PbfFile::open(&out).unwrap();
    assert!(copy.header().is_sorted());
    let b = copy.decode_block(0, DecodeOptions::WITH_METADATA).unwrap();
    let n5 = b.nodes().find(|n| n.id() == 5).unwrap();
    assert_eq!(n5.tags().get("public_transport"), Some("stop"));
    let info = n5.info().unwrap();
    assert_eq!((info.version, info.changeset, info.user()), (3, 5, "ann"));
    // Without metadata.
    let (_, _) = extract(
        &file,
        &ExtractOptions {
            metadata: false,
            ..Default::default()
        },
        std::fs::File::create(&out).unwrap(),
        |_| Flow::Continue,
    )
    .unwrap();
    let b = PbfFile::open(&out)
        .unwrap()
        .decode_block(0, DecodeOptions::WITH_METADATA)
        .unwrap();
    assert!(b.nodes().all(|n| n.info().is_none_or(|i| i.version == 0)));
}

#[test]
fn poly_files() {
    let poly = "region\n1\n 0.4 0.4\n 0.6 0.4\n 0.6 0.6\n 0.4 0.6\n 0.4 0.4\nEND\n!hole\n 0.45 0.45\n 0.55 0.45\n 0.55 0.55\n 0.45 0.45\nEND\nEND\n";
    let r = Region::from_poly(poly).unwrap();
    assert_eq!(r.bbox(), [0.4, 0.4, 0.6, 0.6]);
    let Region::Polygons(mp) = &r else { panic!() };
    assert_eq!(mp.0[0].interiors().len(), 1);
    assert!(Region::from_poly("x\n1\n 0 0\n").is_err());
    assert!(Region::from_geojson(&serde_json::json!({"type": "Point", "coordinates": [0, 0]})).is_err());
}
