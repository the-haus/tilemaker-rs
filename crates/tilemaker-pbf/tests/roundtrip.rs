use std::io::Cursor;
use std::path::PathBuf;

use tilemaker_pbf::*;

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tilemaker-pbf-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn write_sample(opts: WriterOptions, count: i64) -> Vec<u8> {
    let mut w = PbfWriter::with_options(Vec::new(), opts);
    for i in 1..=count {
        let name = format!("n{i}");
        let tags: Vec<(&str, &str)> = if i % 3 == 0 {
            vec![("name", name.as_str()), ("amenity", "cafe")]
        } else {
            vec![]
        };
        w.node(i, (i as i32) * 1000 - 500_000_000, -(i as i32) * 777, &tags)
            .unwrap();
    }
    for i in 1..=count / 10 {
        let refs: Vec<i64> = (0..5).map(|k| (i * 7 + k) % count + 1).collect();
        w.way(100 + i, &refs, &[("highway", "residential")]).unwrap();
    }
    w.relation(
        9,
        &[
            (101, ElementType::Way, "outer"),
            (5, ElementType::Node, "label"),
            (8, ElementType::Relation, ""),
        ],
        &[("type", "multipolygon"), ("name", "Ünïcødé ✓")],
    )
    .unwrap();
    w.finish().unwrap()
}

#[test]
fn round_trip_through_file() {
    let bytes = write_sample(
        WriterOptions {
            sorted: true,
            bbox: Some(HeaderBbox {
                min_lon: -1.5,
                min_lat: -2.5,
                max_lon: 3.25,
                max_lat: 4.0,
            }),
            ..Default::default()
        },
        20_000,
    );
    let path = temp_path("roundtrip.osm.pbf");
    std::fs::write(&path, &bytes).unwrap();

    let file = PbfFile::open(&path).unwrap();
    assert!(file.header().is_sorted());
    assert_eq!(
        file.header().bbox,
        Some(HeaderBbox {
            min_lon: -1.5,
            min_lat: -2.5,
            max_lon: 3.25,
            max_lat: 4.0
        })
    );
    // 20,000 nodes → 3 node blocks; 2,000 ways → 1 block; 1 relation block.
    assert_eq!(file.blobs().len(), 5);

    let mut nodes = 0;
    let mut ways = 0;
    let mut rels = 0;
    let mut last_index = None;
    for b in file.blocks(0..file.blobs().len(), DecodeOptions::ALL, 3) {
        let (i, block) = b.unwrap();
        assert_eq!(last_index.map_or(0, |l: usize| l + 1), i, "blocks arrive in order");
        last_index = Some(i);
        for n in block.nodes() {
            nodes += 1;
            assert_eq!(n.lat_e7(), (n.id() as i32) * 1000 - 500_000_000);
            assert_eq!(n.lon_e7(), -(n.id() as i32) * 777);
            if n.id() % 3 == 0 {
                assert_eq!(n.tags().get("name"), Some(format!("n{}", n.id()).as_str()));
                assert_eq!(n.tags().get("amenity"), Some("cafe"));
            } else {
                assert!(n.tags().is_empty());
            }
        }
        for w in block.ways() {
            ways += 1;
            assert_eq!(w.refs().len(), 5);
            assert_eq!(w.tags().get("highway"), Some("residential"));
            assert!(w.locations().is_empty());
        }
        for r in block.relations() {
            rels += 1;
            assert_eq!(r.id(), 9);
            assert_eq!(r.tags().get("name"), Some("Ünïcødé ✓"));
            let m: Vec<_> = r.members().map(|m| (m.id, m.kind, m.role.to_string())).collect();
            assert_eq!(
                m,
                vec![
                    (101, ElementType::Way, "outer".into()),
                    (5, ElementType::Node, "label".into()),
                    (8, ElementType::Relation, "".into())
                ]
            );
        }
    }
    assert_eq!((nodes, ways, rels), (20_000, 2_000, 1));

    let layout = file.layout().unwrap();
    assert_eq!(
        layout,
        Layout {
            nodes: 0..3,
            ways: 3..4,
            relations: 4..5
        }
    );
}

#[test]
fn unsorted_layout_spans_everything() {
    let bytes = write_sample(WriterOptions::default(), 100);
    let path = temp_path("unsorted.osm.pbf");
    std::fs::write(&path, &bytes).unwrap();
    let file = PbfFile::open(&path).unwrap();
    let n = file.blobs().len();
    assert_eq!(
        file.layout().unwrap(),
        Layout {
            nodes: 0..n,
            ways: 0..n,
            relations: 0..n
        }
    );
}

#[test]
fn blob_reader_streams() {
    let bytes = write_sample(WriterOptions::default(), 100);
    let mut kinds = Vec::new();
    let mut total = 0;
    for b in BlobReader::new(Cursor::new(&bytes)) {
        let (info, body) = b.unwrap();
        kinds.push(info.kind);
        if info.kind == BlobKind::Data {
            let mut out = Vec::new();
            decompress_blob(&body, &mut out).unwrap();
            let block = PrimitiveBlock::decode(&out, DecodeOptions::ALL).unwrap();
            total += block.node_count() + block.way_count() + block.relation_count();
        }
    }
    assert_eq!(kinds[0], BlobKind::Header);
    assert_eq!(total, 100 + 10 + 1);
}

#[test]
fn uncompressed_blobs() {
    let bytes = write_sample(
        WriterOptions {
            uncompressed: true,
            ..Default::default()
        },
        50,
    );
    let mut total = 0;
    for b in BlobReader::new(Cursor::new(&bytes)) {
        let (info, body) = b.unwrap();
        if info.kind == BlobKind::Data {
            let mut out = Vec::new();
            decompress_blob(&body, &mut out).unwrap();
            total += PrimitiveBlock::decode(&out, DecodeOptions::ALL).unwrap().node_count();
        }
    }
    assert_eq!(total, 50);
}

#[test]
fn metadata_round_trips() {
    let mut w = PbfWriter::new(Vec::new());
    let info = Info {
        version: 3,
        timestamp: 1_700_000_000,
        changeset: 42,
        uid: 7,
        visible: true,
    };
    w.node_with_info(1, 0, 0, &[("a", "b")], Some((&info, "alice")))
        .unwrap();
    let info2 = Info {
        version: 1,
        timestamp: 1_600_000_000,
        changeset: 41,
        uid: 8,
        visible: true,
    };
    w.node_with_info(2, 1, 1, &[], Some((&info2, "bob"))).unwrap();
    w.way_full(10, &[1, 2], &[(0, 0), (1, 1)], &[], Some((&info, "carol")))
        .unwrap();
    w.relation_with_info(20, &[(10, ElementType::Way, "x")], &[], Some((&info2, "dave")))
        .unwrap();
    let bytes = w.finish().unwrap();

    let mut seen = Vec::new();
    for b in BlobReader::new(Cursor::new(&bytes)) {
        let (info, body) = b.unwrap();
        if info.kind != BlobKind::Data {
            continue;
        }
        let mut out = Vec::new();
        decompress_blob(&body, &mut out).unwrap();
        let block = PrimitiveBlock::decode(&out, DecodeOptions::WITH_METADATA).unwrap();
        for n in block.nodes() {
            let i = n.info().unwrap();
            seen.push((n.id(), i.version, i.timestamp, i.changeset, i.uid, i.user().to_string()));
        }
        for wy in block.ways() {
            let i = wy.info().unwrap();
            seen.push((
                wy.id(),
                i.version,
                i.timestamp,
                i.changeset,
                i.uid,
                i.user().to_string(),
            ));
            assert_eq!(wy.locations(), &[(0, 0), (1, 1)]);
        }
        for r in block.relations() {
            let i = r.info().unwrap();
            seen.push((r.id(), i.version, i.timestamp, i.changeset, i.uid, i.user().to_string()));
        }
        // Without the metadata option nothing is decoded.
        let plain = PrimitiveBlock::decode(&out, DecodeOptions::ALL).unwrap();
        assert!(plain.nodes().all(|n| n.info().is_none()));
    }
    assert_eq!(
        seen,
        vec![
            (1, 3, 1_700_000_000, 42, 7, "alice".into()),
            (2, 1, 1_600_000_000, 41, 8, "bob".into()),
            (10, 3, 1_700_000_000, 42, 7, "carol".into()),
            (20, 1, 1_600_000_000, 41, 8, "dave".into()),
        ]
    );
}

#[test]
fn negative_ids_and_extreme_coordinates() {
    let mut w = PbfWriter::new(Vec::new());
    w.node(-5, -900_000_000, -1_800_000_000, &[]).unwrap();
    w.node(14_274_176_831, 900_000_000, 1_800_000_000, &[]).unwrap();
    let bytes = w.finish().unwrap();
    let mut got = Vec::new();
    for b in BlobReader::new(Cursor::new(&bytes)) {
        let (info, body) = b.unwrap();
        if info.kind == BlobKind::Data {
            let mut out = Vec::new();
            decompress_blob(&body, &mut out).unwrap();
            for n in PrimitiveBlock::decode(&out, DecodeOptions::ALL).unwrap().nodes() {
                got.push((n.id(), n.lat(), n.lon()));
            }
        }
    }
    assert_eq!(got, vec![(-5, -90.0, -180.0), (14_274_176_831, 90.0, 180.0)]);
}

#[test]
fn skipping_kinds_still_reports_them() {
    let bytes = write_sample(WriterOptions::default(), 10);
    for b in BlobReader::new(Cursor::new(&bytes)) {
        let (info, body) = b.unwrap();
        if info.kind != BlobKind::Data {
            continue;
        }
        let mut out = Vec::new();
        decompress_blob(&body, &mut out).unwrap();
        let none = DecodeOptions {
            nodes: false,
            ways: false,
            relations: false,
            metadata: false,
        };
        let block = PrimitiveBlock::decode(&out, none).unwrap();
        assert_eq!(block.node_count() + block.way_count() + block.relation_count(), 0);
        let k = block.kinds();
        assert!(k.nodes || k.ways || k.relations);
    }
}

#[test]
fn corrupt_input_is_an_error_not_a_panic() {
    let bytes = write_sample(WriterOptions::default(), 200);
    // Truncate at many points; flip bytes at many points.
    for cut in (1..bytes.len()).step_by(37) {
        let r: Result<Vec<_>, _> = BlobReader::new(Cursor::new(&bytes[..cut])).collect();
        let _ = r;
    }
    for pos in (0..bytes.len()).step_by(13) {
        let mut b = bytes.clone();
        b[pos] ^= 0x5a;
        for blob in BlobReader::new(Cursor::new(&b)) {
            let Ok((info, body)) = blob else { break };
            if info.kind == BlobKind::Data {
                let mut out = Vec::new();
                if decompress_blob(&body, &mut out).is_ok() {
                    let _ = PrimitiveBlock::decode(&out, DecodeOptions::WITH_METADATA);
                }
            }
        }
    }
    let path = temp_path("truncated.osm.pbf");
    std::fs::write(&path, &bytes[..bytes.len() - 10]).unwrap();
    assert!(PbfFile::open(&path).is_err());
}

#[test]
fn missing_header_is_an_error() {
    let path = temp_path("empty.osm.pbf");
    std::fs::write(&path, b"").unwrap();
    assert!(matches!(PbfFile::open(&path), Err(Error::Corrupt(_))));
}
