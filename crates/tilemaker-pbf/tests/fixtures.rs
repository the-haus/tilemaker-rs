//! Checks against real extracts (counts from `osmium fileinfo -e`). The
//! fixtures are not committed; the tests skip when they are absent.

use std::path::PathBuf;

use tilemaker_pbf::*;

fn fixture(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name);
    p.exists().then_some(p)
}

fn counts(path: &PathBuf, threads: usize) -> (usize, usize, usize, usize) {
    let file = PbfFile::open(path).unwrap();
    let (mut n, mut w, mut r, mut tagged) = (0, 0, 0, 0);
    for b in file.blocks(0..file.blobs().len(), DecodeOptions::ALL, threads) {
        let (_, block) = b.unwrap();
        n += block.node_count();
        w += block.way_count();
        r += block.relation_count();
        tagged += block.nodes().filter(|n| n.tags().contains_key("amenity")).count();
    }
    (n, w, r, tagged)
}

#[test]
fn monaco_counts_match_osmium() {
    let Some(p) = fixture("monaco.osm.pbf") else { return };
    assert_eq!(counts(&p, 1), (97_858, 14_715, 451, 1_311));
    assert_eq!(counts(&p, 8), (97_858, 14_715, 451, 1_311));
    let f = PbfFile::open(&p).unwrap();
    assert!(f.header().is_sorted());
    let b = f.header().bbox.unwrap();
    assert!((b.min_lon - 7.40).abs() < 1e-9 && (b.max_lat - 43.76).abs() < 1e-9);
    let layout = f.layout().unwrap();
    assert_eq!(layout.relations.end, f.blobs().len());
    assert!(layout.nodes.end <= layout.ways.start + 1);
    // Probing first elements agrees with decoding.
    for i in 0..f.blobs().len() {
        let b = f.decode_block(i, DecodeOptions::ALL).unwrap();
        let want = b
            .nodes()
            .next()
            .map(|n| (ElementType::Node, n.id()))
            .or_else(|| b.ways().next().map(|w| (ElementType::Way, w.id())))
            .or_else(|| b.relations().next().map(|r| (ElementType::Relation, r.id())));
        assert_eq!(f.first_element(i).unwrap(), want, "block {i}");
    }
}

#[test]
fn liechtenstein_counts_match_osmium() {
    let Some(p) = fixture("liechtenstein.osm.pbf") else {
        return;
    };
    let (n, w, r, _) = counts(&p, 4);
    assert_eq!((n, w, r), (749_264, 100_236, 1_798));
}
