use std::io::Cursor;
use std::path::PathBuf;

use tilemaker_pbf::write::ELEMENTS_PER_BLOCK;
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

/// Frame a raw (uncompressed) blob.
fn raw_blob(kind: &str, data: &[u8]) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    let mut blob = vec![0x0a];
    varint(&mut blob, data.len() as u64);
    blob.extend_from_slice(data);
    let mut header = vec![0x0a];
    varint(&mut header, kind.len() as u64);
    header.extend_from_slice(kind.as_bytes());
    header.push(0x18);
    varint(&mut header, blob.len() as u64);
    let mut out = (header.len() as u32).to_be_bytes().to_vec();
    out.extend(header);
    out.extend(blob);
    out
}

#[test]
fn empty_blocks_do_not_break_the_layout() {
    // A sorted file: node blocks, an empty block, more node blocks, ways.
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            ..Default::default()
        },
    );
    for i in 1..=24_000i64 {
        w.node(i, 0, 0, &[]).unwrap();
    }
    w.flush().unwrap();
    let mut first = w.finish().unwrap();
    // An empty PrimitiveBlock: just a string table with "".
    first.extend(raw_blob("OSMData", &[0x0a, 0x02, 0x0a, 0x00]));
    let mut w = PbfWriter::with_options(
        Vec::new(),
        WriterOptions {
            sorted: true,
            ..Default::default()
        },
    );
    for i in 24_001..=40_000i64 {
        w.node(i, 0, 0, &[]).unwrap();
    }
    w.way(1, &[1, 2], &[("highway", "x")]).unwrap();
    let second = w.finish().unwrap();
    // Drop the second file's header blob.
    let hlen = u32::from_be_bytes(second[..4].try_into().unwrap()) as usize;
    let mut r = BlobReader::new(Cursor::new(&second));
    let (info, _) = r.next_blob().unwrap().unwrap();
    let skip = 4 + hlen + info.size as usize;
    first.extend_from_slice(&second[skip..]);

    let path = temp_path("empty-block.osm.pbf");
    std::fs::write(&path, &first).unwrap();
    let file = PbfFile::open(&path).unwrap();
    let layout = file.layout().unwrap();
    let ways: usize = file
        .blocks(layout.ways.clone(), DecodeOptions::ALL, 2)
        .map(|b| b.unwrap().1.way_count())
        .sum();
    assert_eq!(ways, 1, "{layout:?}");
    let nodes: usize = file
        .blocks(layout.nodes.clone(), DecodeOptions::ALL, 2)
        .map(|b| b.unwrap().1.node_count())
        .sum();
    assert_eq!(nodes, 40_000, "{layout:?}");
}

/// Re-frame every data blob of an uncompressed file with another codec:
/// field 7 (zstd) or 6 (lz4), with raw_size.
fn recompress(bytes: &[u8], field: u8, compress: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    let mut out = Vec::new();
    for b in BlobReader::new(Cursor::new(bytes)) {
        let (info, body) = b.unwrap();
        let mut raw = Vec::new();
        decompress_blob(&body, &mut raw).unwrap();
        let kind = if info.kind == BlobKind::Header {
            "OSMHeader"
        } else {
            "OSMData"
        };
        let packed = compress(&raw);
        let mut blob = vec![0x10];
        varint(&mut blob, raw.len() as u64);
        blob.push((field << 3) | 2);
        varint(&mut blob, packed.len() as u64);
        blob.extend_from_slice(&packed);
        let mut header = vec![0x0a];
        varint(&mut header, kind.len() as u64);
        header.extend_from_slice(kind.as_bytes());
        header.push(0x18);
        varint(&mut header, blob.len() as u64);
        out.extend_from_slice(&(header.len() as u32).to_be_bytes());
        out.extend(header);
        out.extend(blob);
    }
    out
}

#[test]
fn zstd_and_lz4_blobs() {
    let plain = write_sample(WriterOptions::default(), 5_000);
    let count = |bytes: &[u8]| -> usize {
        let path = temp_path(&format!("codec-{}.osm.pbf", bytes.len()));
        std::fs::write(&path, bytes).unwrap();
        let f = PbfFile::open(&path).unwrap();
        f.blocks(0..f.blobs().len(), DecodeOptions::ALL, 2)
            .map(|b| {
                let (_, b) = b.unwrap();
                b.node_count() + b.way_count() + b.relation_count()
            })
            .sum()
    };
    let want = count(&plain);
    assert_eq!(want, 5_000 + 500 + 1);
    let z = recompress(&plain, 7, |d| zstd::bulk::compress(d, 3).unwrap());
    assert_eq!(count(&z), want);
    let l = recompress(&plain, 6, lz4_flex::block::compress);
    assert_eq!(count(&l), want);
}

/// The first element of every block, as decoding finds it.
fn decoded_firsts(file: &PbfFile) -> Vec<Option<(ElementType, i64)>> {
    (0..file.blobs().len())
        .map(|i| {
            let b = file.decode_block(i, DecodeOptions::ALL).unwrap();
            let first = b
                .nodes()
                .next()
                .map(|n| (ElementType::Node, n.id()))
                .or_else(|| b.ways().next().map(|w| (ElementType::Way, w.id())))
                .or_else(|| b.relations().next().map(|r| (ElementType::Relation, r.id())));
            first
        })
        .collect()
}

#[test]
fn first_elements_from_partial_inflation() {
    for (name, opts) in [
        (
            "firsts.osm.pbf",
            WriterOptions {
                sorted: true,
                ..Default::default()
            },
        ),
        (
            "firsts-raw.osm.pbf",
            WriterOptions {
                sorted: true,
                uncompressed: true,
                ..Default::default()
            },
        ),
    ] {
        let path = temp_path(name);
        std::fs::write(&path, write_sample(opts, 30_000)).unwrap();
        let file = PbfFile::open(&path).unwrap();
        let want = decoded_firsts(&file);
        let got: Vec<_> = (0..file.blobs().len())
            .map(|i| file.first_element(i).unwrap())
            .collect();
        assert_eq!(got, want);
        assert!(got.iter().any(|f| matches!(f, Some((ElementType::Way, _)))));
        // Blocks whose id range holds a wanted id.
        let layout = file.layout().unwrap();
        let ways = file
            .blocks_with_ids(layout.ways.clone(), ElementType::Way, 2, |lo, hi| {
                lo <= 1500 && 1500 <= hi
            })
            .unwrap();
        assert_eq!(ways.len(), 1);
        let b = file.decode_block(ways[0], DecodeOptions::ALL).unwrap();
        assert!(b.ways().any(|w| w.id() == 1500));
        let nodes = file
            .blocks_with_ids(layout.nodes.clone(), ElementType::Node, 2, |_, _| false)
            .unwrap();
        assert!(nodes.is_empty());
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn parallel_writer_merges_blocks_in_order() {
    let path = temp_path("parallel.osm.pbf");
    let info = Info {
        version: 4,
        timestamp: 1_700_000_000,
        changeset: 77,
        uid: 12,
        visible: true,
    };
    let f = std::fs::File::create(&path).unwrap();
    let mut w = ParallelWriter::new(
        std::io::BufWriter::new(f),
        WriterOptions {
            sorted: true,
            ..Default::default()
        },
        4,
    )
    .unwrap();
    // Many small builders, as an extract's selections are.
    let mut id = 0i64;
    for chunk in 0..900 {
        let mut b = BlockBuilder::new();
        for _ in 0..(chunk % 37) {
            id += 1;
            let name = format!("n{id}");
            b.node(
                id,
                id as i32,
                -(id as i32),
                [("name", name.as_str())],
                Some((&info, "eve")),
            );
        }
        w.push(b).unwrap();
    }
    let nodes = id;
    let mut b = BlockBuilder::new();
    b.way(1, &[1, 2, 3], &[], [("highway", "x")], None);
    w.push(b).unwrap();
    let mut b = BlockBuilder::new();
    b.relation(
        1,
        [(1, ElementType::Way, "outer")],
        [("type", "multipolygon")],
        Some((&info, "eve")),
    );
    w.push(b).unwrap();
    let (_, stats) = w.finish().unwrap();
    assert_eq!((stats.nodes, stats.ways, stats.relations), (nodes as u64, 1, 1));
    assert_eq!(stats.bytes, std::fs::metadata(&path).unwrap().len());

    let file = PbfFile::open(&path).unwrap();
    assert!(file.header().is_sorted());
    let mut next = 1i64;
    let mut ways = 0;
    let mut rels = 0;
    for r in file.blocks(0..file.blobs().len(), DecodeOptions::WITH_METADATA, 2) {
        let (_, b) = r.unwrap();
        assert!(b.node_count() <= ELEMENTS_PER_BLOCK);
        let kinds = b.kinds();
        assert!(
            [kinds.nodes, kinds.ways, kinds.relations]
                .iter()
                .filter(|&&k| k)
                .count()
                == 1
        );
        for n in b.nodes() {
            assert_eq!(n.id(), next);
            assert_eq!(n.tags().get("name"), Some(format!("n{next}").as_str()));
            assert_eq!(n.info().unwrap().changeset, 77);
            assert_eq!(n.info().unwrap().user(), "eve");
            next += 1;
        }
        ways += b.way_count();
        for r in b.relations() {
            rels += 1;
            assert_eq!(r.info().unwrap().timestamp, 1_700_000_000);
        }
    }
    assert_eq!((next - 1, ways, rels), (nodes, 1, 1));
    // Merged: far fewer blocks than builders.
    assert!(file.blobs().len() < 10, "{}", file.blobs().len());
    let _ = std::fs::remove_file(path);
}
