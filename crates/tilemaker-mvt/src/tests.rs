use super::*;

fn single_layer(tile: &TileBuilder) -> DecodedLayer {
    let decoded = decode(&tile.encode()).expect("decode");
    assert_eq!(decoded.layers.len(), 1);
    decoded.layers.into_iter().next().unwrap()
}

#[test]
fn spec_point_example() {
    let mut tile = TileBuilder::new();
    let mut f = tile.layer("points", 2, 4096).feature(GeomType::Point);
    f.add_points(&[(25, 17)]);
    assert!(f.commit());
    let layer = single_layer(&tile);
    assert_eq!(layer.features[0].geometry, vec![9, 50, 34]);
    assert_eq!(layer.features[0].geom_type, GeomType::Point);
    assert_eq!(layer.version, 2);
    assert_eq!(layer.extent, 4096);
}

#[test]
fn spec_multipoint_and_linestring_examples() {
    let mut tile = TileBuilder::new();
    let layer = tile.layer("l", 2, 4096);
    let mut f = layer.feature(GeomType::Point);
    f.add_points(&[(5, 7), (3, 2)]);
    f.commit();
    let mut f = layer.feature(GeomType::LineString);
    f.add_linestring(&[(2, 2), (2, 10), (10, 10)]);
    f.commit();
    let layer = single_layer(&tile);
    assert_eq!(layer.features[0].geometry, vec![17, 10, 14, 3, 9]);
    assert_eq!(layer.features[1].geometry, vec![9, 4, 4, 18, 0, 16, 16, 0]);
}

#[test]
fn spec_polygon_example() {
    let mut tile = TileBuilder::new();
    let mut f = tile.layer("p", 2, 4096).feature(GeomType::Polygon);
    f.add_ring(&[(3, 6), (8, 12), (20, 34), (3, 6)]);
    assert!(f.commit());
    let layer = single_layer(&tile);
    assert_eq!(layer.features[0].geometry, vec![9, 6, 12, 18, 10, 12, 24, 44, 15]);
    assert_eq!(
        layer.features[0].decode_geometry(),
        vec![vec![(3, 6), (8, 12), (20, 34), (3, 6)]]
    );
}

#[test]
fn roundtrip_multi_geometries_cursor_carries() {
    let mut tile = TileBuilder::new();
    let layer = tile.layer("roads", 2, 4096);
    let mut f = layer.feature(GeomType::LineString);
    f.set_id(42);
    f.add_linestring(&[(0, 0), (10, 0), (10, 0), (10, 10)]);
    f.add_linestring(&[(-5, -5), (-100, 4200)]);
    f.add_linestring(&[(7, 7), (7, 7)]); // degenerate: skipped
    assert!(f.commit());

    let mut f = layer.feature(GeomType::Polygon);
    f.add_ring(&[(0, 0), (100, 0), (100, 100), (0, 100)]); // open ring
    f.add_ring(&[(10, 10), (10, 20), (20, 20), (20, 10), (10, 10)]);
    f.add_ring(&[(1, 1), (2, 2), (1, 1)]); // too few points: skipped
    assert!(f.commit());

    let layer = single_layer(&tile);
    assert_eq!(layer.features.len(), 2);
    assert_eq!(layer.features[0].id, Some(42));
    assert_eq!(
        layer.features[0].decode_geometry(),
        vec![vec![(0, 0), (10, 0), (10, 10)], vec![(-5, -5), (-100, 4200)]]
    );
    assert_eq!(layer.features[1].id, None);
    assert_eq!(
        layer.features[1].decode_geometry(),
        vec![
            vec![(0, 0), (100, 0), (100, 100), (0, 100), (0, 0)],
            vec![(10, 10), (10, 20), (20, 20), (20, 10), (10, 10)],
        ]
    );
}

#[test]
fn empty_features_and_layers() {
    let mut tile = TileBuilder::new();
    assert!(tile.is_empty());
    let layer = tile.layer("empty", 2, 4096);
    let mut f = layer.feature(GeomType::LineString);
    f.add_linestring(&[(1, 1)]);
    assert!(!f.commit());
    let mut f = layer.feature(GeomType::Point);
    f.add_points(&[(1, 1)]);
    drop(f); // discarded
    assert_eq!(layer.len(), 0);
    assert!(tile.is_empty());
    assert!(tile.encode().is_empty());

    let mut f = tile.layer("full", 2, 4096).feature(GeomType::Point);
    f.add_points(&[(1, 1)]);
    f.commit();
    assert!(!tile.is_empty());
    let decoded = decode(&tile.encode()).unwrap();
    assert_eq!(decoded.layers.len(), 1);
    assert_eq!(decoded.layers[0].name, "full");
}

#[test]
fn layer_lookup_reuses_existing() {
    let mut tile = TileBuilder::new();
    for name in ["a", "b", "a"] {
        let mut f = tile.layer(name, 2, 4096).feature(GeomType::Point);
        f.add_points(&[(1, 2)]);
        f.commit();
    }
    let decoded = decode(&tile.encode()).unwrap();
    let names: Vec<_> = decoded.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(decoded.layers[0].features.len(), 2);
    assert_eq!(tile.layer("a", 2, 4096).name(), "a");
}

#[test]
fn properties_roundtrip_and_dedupe() {
    let mut tile = TileBuilder::new();
    let layer = tile.layer("poi", 2, 4096);
    let props = vec![
        ("name", Value::String("Café".into())),
        ("f", Value::Float(1.5)),
        ("d", Value::Double(-2.25)),
        ("i", Value::Int(-7)),
        ("u", Value::UInt(u64::MAX)),
        ("s", Value::SInt(-123456789)),
        ("b", Value::Bool(true)),
    ];
    for _ in 0..3 {
        let mut f = layer.feature(GeomType::Point);
        for (k, v) in &props {
            f.add_property(k, v.clone());
        }
        // Same numeric bits but different kind must not be merged.
        f.add_property("x", Value::Int(1));
        f.add_property("y", Value::UInt(1));
        f.add_points(&[(1, 1)]);
        f.commit();
    }
    let layer = single_layer(&tile);
    assert_eq!(layer.keys.len(), 9);
    assert_eq!(layer.values.len(), 9);
    for feat in &layer.features {
        let p = feat.properties(&layer);
        assert_eq!(p.len(), 9);
        for (i, (k, v)) in props.iter().enumerate() {
            assert_eq!(p[i], (*k, v));
        }
        assert_eq!(p[7], ("x", &Value::Int(1)));
        assert_eq!(p[8], ("y", &Value::UInt(1)));
    }
}

#[test]
fn copy_decoded_feature() {
    let mut tile = TileBuilder::new();
    let mut f = tile.layer("w", 2, 4096).feature(GeomType::Polygon);
    f.set_id(9);
    f.add_property("k", Value::Bool(false));
    f.add_ring(&[(0, 0), (5, 0), (5, 5), (0, 0)]);
    f.commit();
    let src = single_layer(&tile);

    let mut out = TileBuilder::new();
    let l = out.layer("w", 2, 4096);
    assert!(l.add_decoded_feature(&src.features[0], &src));
    let copied = single_layer(&out);
    assert_eq!(copied, src);
}

#[test]
fn decode_rejects_truncated() {
    let mut tile = TileBuilder::new();
    let mut f = tile.layer("x", 2, 4096).feature(GeomType::Point);
    f.add_points(&[(1, 1)]);
    f.commit();
    let bytes = tile.encode();
    assert!(decode(&bytes[..bytes.len() - 1]).is_err());
}
