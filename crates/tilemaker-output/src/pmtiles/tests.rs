use super::reader::PmtilesReader;
use super::*;
use crate::test_util::temp_path;

#[test]
fn hilbert_known_values() {
    assert_eq!(zxy_to_tileid(0, 0, 0), 0);
    assert_eq!(zxy_to_tileid(1, 0, 0), 1);
    assert_eq!(zxy_to_tileid(1, 0, 1), 2);
    assert_eq!(zxy_to_tileid(1, 1, 1), 3);
    assert_eq!(zxy_to_tileid(1, 1, 0), 4);
    assert_eq!(zxy_to_tileid(2, 0, 0), 5);
    assert_eq!(zxy_to_tileid(12, 3423, 1763), 19078479);
}

#[test]
fn hilbert_inverse() {
    for z in 0..8u8 {
        let n = 1u32 << z;
        for x in 0..n {
            for y in 0..n {
                assert_eq!(tileid_to_zxy(zxy_to_tileid(z, x, y)), (z, x, y));
            }
        }
    }
    assert_eq!(tileid_to_zxy(zxy_to_tileid(20, 524287, 1)), (20, 524287, 1));
}

#[test]
fn directory_roundtrip() {
    let entries = vec![
        Entry {
            tile_id: 0,
            offset: 0,
            length: 10,
            run_length: 1,
        },
        Entry {
            tile_id: 1,
            offset: 10,
            length: 5,
            run_length: 3,
        },
        Entry {
            tile_id: 9,
            offset: 0,
            length: 10,
            run_length: 1,
        },
        Entry {
            tile_id: 100,
            offset: 1000,
            length: 7,
            run_length: 0,
        },
    ];
    let b = serialize_directory(&entries);
    assert_eq!(deserialize_directory(&b).unwrap(), entries);
}

#[test]
fn header_roundtrip() {
    let h = Header {
        root_dir_offset: 127,
        tile_data_bytes: 99,
        clustered: true,
        min_lon_e7: -1_800_000_000,
        center_lat_e7: 123,
        center_zoom: 4,
        ..Default::default()
    };
    let b = h.serialize();
    assert_eq!(b.len(), 127);
    assert_eq!(Header::deserialize(&b).unwrap(), h);
}

/// Deterministic pseudo-random generator for test data.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn meta() -> TilesetMetadata {
    TilesetMetadata {
        name: "pm".into(),
        version: "3.0".into(),
        description: "d".into(),
        minzoom: 0,
        maxzoom: 6,
        bounds: Some([-10.0, 40.0, 10.0, 60.0]),
        center: None,
        vector_layers_json: r#"[{"id":"roads","fields":{"name":"String"}}]"#.into(),
        extra: vec![("attribution".into(), json!("OSM"))],
    }
}

#[test]
fn roundtrip_with_duplicates_and_rle() {
    let path = temp_path("rt.pmtiles");
    let mut w = PmtilesWriter::create(&path, Compression::Gzip).unwrap();

    // All tiles z0..=4, plus a block on z6, written in scrambled order.
    let mut tiles: Vec<(u8, u32, u32, Vec<u8>)> = Vec::new();
    for z in 0..=4u8 {
        for x in 0..(1u32 << z) {
            for y in 0..(1u32 << z) {
                // Every third tile on z4 is "sea" (shared content).
                let data = if z == 4 && (x + y) % 3 == 0 {
                    b"sea".to_vec()
                } else {
                    format!("tile {z}/{x}/{y} {}", "x".repeat((x * 37 + y) as usize % 300)).into_bytes()
                };
                tiles.push((z, x, y, data));
            }
        }
    }
    for x in 0..8u32 {
        for y in 0..8u32 {
            tiles.push((6, x, y, b"land".to_vec()));
        }
    }
    let mut rng = Lcg(7);
    for i in (1..tiles.len()).rev() {
        tiles.swap(i, rng.next() as usize % (i + 1));
    }
    for (z, x, y, d) in &tiles {
        w.write_tile(*z, *x, *y, d).unwrap();
    }
    // Rewriting a tile replaces it.
    w.write_tile(0, 0, 0, b"root v2").unwrap();
    w.finish(&meta()).unwrap();
    drop(w);
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    assert!(!PathBuf::from(tmp).exists());

    let r = PmtilesReader::open(&path).unwrap();
    let h = r.header().clone();
    assert!(h.clustered);
    assert_eq!(h.tile_compression, COMPRESSION_GZIP);
    assert_eq!(h.internal_compression, COMPRESSION_GZIP);
    assert_eq!(h.tile_type, TILE_TYPE_MVT);
    assert_eq!(h.addressed_tiles_count, tiles.len() as u64);
    let sea = tiles.iter().filter(|t| t.3 == b"sea").count() as u64;
    let distinct = tiles.len() as u64 - sea - 64 + 2;
    assert_eq!(h.tile_contents_count, distinct);
    assert!(h.tile_entries_count < h.addressed_tiles_count);
    assert_eq!(h.min_zoom, 0);
    assert_eq!(h.max_zoom, 6);
    assert_eq!(h.min_lon_e7, -100_000_000);
    assert_eq!(h.max_lat_e7, 600_000_000);
    assert_eq!(h.center_lat_e7, 500_000_000);
    assert_eq!(h.center_zoom, 3);
    assert_eq!(h.leaf_dirs_bytes, 0);
    assert_eq!(
        h.tile_data_offset + h.tile_data_bytes,
        std::fs::metadata(&path).unwrap().len()
    );

    for (z, x, y, d) in &tiles {
        let got = r.get_tile_decompressed(*z, *x, *y).unwrap().unwrap();
        if (*z, *x, *y) == (0, 0, 0) {
            assert_eq!(got, b"root v2");
        } else {
            assert_eq!(&got, d, "tile {z}/{x}/{y}");
        }
    }
    assert_eq!(r.get_tile(5, 0, 0).unwrap(), None);
    assert_eq!(r.get_tile(6, 8, 8).unwrap(), None);

    // Entries are sorted and tile data offsets are clustered (ascending).
    let entries = r.entries().unwrap();
    assert_eq!(entries.len() as u64, h.tile_entries_count);
    assert!(entries.windows(2).all(|w| w[0].tile_id < w[1].tile_id));
    let mut max_end = 0;
    for e in &entries {
        assert!(e.offset <= max_end, "new content must follow previous content");
        max_end = max_end.max(e.offset + u64::from(e.length));
    }

    let md = r.metadata().unwrap();
    assert_eq!(md["name"], "pm");
    assert_eq!(md["attribution"], "OSM");
    assert_eq!(md["vector_layers"][0]["id"], "roads");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn leaf_directories() {
    let path = temp_path("leaves.pmtiles");
    let mut w = PmtilesWriter::create(&path, Compression::None).unwrap();
    let mut rng = Lcg(42);
    let mut written = Vec::new();
    let n = 1u32 << 8;
    for x in 0..n {
        for y in 0..n {
            if !rng.next().is_multiple_of(3) {
                // Unique content with pseudo-random length (poorly compressible directory).
                let len = (rng.next() % 200) as usize + 8;
                let mut data = vec![0u8; len];
                data[..8].copy_from_slice(&(u64::from(x) << 32 | u64::from(y)).to_le_bytes());
                w.write_tile(8, x, y, &data).unwrap();
                written.push((x, y, data));
            }
        }
    }
    w.finish(&meta()).unwrap();
    drop(w);

    let r = PmtilesReader::open(&path).unwrap();
    let h = r.header().clone();
    assert!(h.leaf_dirs_bytes > 0, "expected leaf directories");
    assert!(h.root_dir_bytes as usize <= 16384 - 127);
    assert_eq!(h.tile_compression, COMPRESSION_NONE);
    assert_eq!(h.addressed_tiles_count, written.len() as u64);
    for (x, y, d) in written.iter().step_by(7) {
        assert_eq!(&r.get_tile(8, *x, *y).unwrap().unwrap(), d);
    }
    assert_eq!(r.entries().unwrap().len() as u64, h.tile_entries_count);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn empty_archive() {
    let path = temp_path("empty.pmtiles");
    let mut w = PmtilesWriter::create(&path, Compression::Deflate).unwrap();
    w.finish(&TilesetMetadata::default()).unwrap();
    let r = PmtilesReader::open(&path).unwrap();
    assert_eq!(r.header().addressed_tiles_count, 0);
    assert_eq!(r.get_tile(0, 0, 0).unwrap(), None);
    std::fs::remove_file(&path).unwrap();
}
