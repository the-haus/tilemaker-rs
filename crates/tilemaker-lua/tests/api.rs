use std::sync::Arc;

use tilemaker_config::Config;
use tilemaker_geom::LatpLon;
use tilemaker_lua::*;

const CONFIG: &str = r#"{
  "layers": {
    "poi": { "minzoom": 12, "maxzoom": 14 },
    "roads": { "minzoom": 4, "maxzoom": 14 },
    "water": { "minzoom": 6, "maxzoom": 14 },
    "boundary": { "minzoom": 0, "maxzoom": 14 }
  },
  "settings": { "minzoom": 0, "maxzoom": 14, "basezoom": 14, "name": "Test", "version": "1", "description": "", "compress": "gzip" }
}"#;

fn ll(lon: f64, lat: f64) -> LatpLon {
    LatpLon::from_degrees(lat, lon)
}

fn profile(src: &str) -> (LuaProfile, Arc<Shared>) {
    let config = Config::from_json(CONFIG).unwrap();
    let shared = Shared::new(&config);
    (LuaProfile::load(src, "test.lua", shared.clone(), true).unwrap(), shared)
}

fn tags(t: &[(&str, &str)]) -> Vec<(String, String)> {
    t.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

const PROFILE: &str = r#"
node_keys = { "amenity", "place" }
way_keys = { "highway", "natural" }
inits = 0
function init_function(name, first) inits = inits + 1; assert(name == "Test"); assert(first == true) end
function node_function(node)
  local amenity = Find("amenity")
  if amenity ~= "" then
    Layer("poi")
    Attribute("class", amenity)
    Attribute("name", Find("name"), 14)
    AttributeInteger("rank", 3)
    AttributeNumeric("height", 12.5)
    AttributeBoolean("open", Holds("opening_hours"))
    Attribute("class", "replaced")
    MinZoom(13)
    ZOrder(2)
    -- the old method style still works
    assert(node:Find("amenity") == amenity)
    assert(OsmType() == "node" and Id() == "42")
    assert(not IsClosed())
  end
end
function way_function()
  if Find("highway") ~= "" then
    Layer("roads", false)
    Attribute("class", Find("highway"))
    Layer("poi")  -- a second output reusing nothing (point needs LayerAsCentroid)
  end
  if Find("natural") == "water" then
    Layer("water", true)
    AttributeNumeric("area", Area())
    LayerAsCentroid("poi")
    Attribute("name", "lake")
    local c = Centroid()
    SetData("centroid_lat", tostring(c[1]))
  end
  if IsMultiPolygon() then
    Layer("water", true)
    Attribute("kind", "mp")
  end
end
function relation_scan_function()
  if Find("type") == "boundary" then Accept() end
end
function relation_function()
  if Find("boundary") == "administrative" then
    Layer("boundary", false)
    Attribute("admin_level", Find("admin_level"))
  end
end
"#;

#[test]
fn capabilities() {
    let (p, _) = profile(PROFILE);
    let c = p.capabilities();
    assert!(c.nodes && c.ways && c.relations && c.relation_scan && !c.relation_postscan);
    assert_eq!(
        c.node_keys.as_deref(),
        Some(&["amenity".to_string(), "place".to_string()][..])
    );
    assert_eq!(
        c.way_keys.as_deref(),
        Some(&["highway".to_string(), "natural".to_string()][..])
    );
}

#[test]
fn node_outputs_and_attributes() {
    let (mut p, shared) = profile(PROFILE);
    let out = p
        .process(Object {
            kind: ObjectKind::Node,
            id: 42,
            tags: tags(&[("amenity", "cafe"), ("name", "Bob's")]),
            location: Some(ll(1.0, 2.0)),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(out.outputs.len(), 1);
    let o = &out.outputs[0];
    assert_eq!(o.layer, 0);
    assert_eq!(o.min_zoom, 13);
    assert_eq!(o.z_order, 2.0);
    let attrs: Vec<_> = o
        .attributes
        .iter()
        .map(|a| (a.key.as_str(), a.value.clone(), a.min_zoom))
        .collect();
    assert_eq!(
        attrs,
        vec![
            ("name", AttributeValue::String("Bob's".into()), 14),
            ("rank", AttributeValue::Int(3), 0),
            ("height", AttributeValue::Float(12.5), 0),
            ("open", AttributeValue::Bool(false), 0),
            ("class", AttributeValue::String("replaced".into()), 0),
        ]
    );
    assert!(matches!(out.geometries[0], OutputGeometry::Point(_)));
    let fields = shared.fields.lock().unwrap();
    assert_eq!(fields[0].get("rank"), Some(&1));
    assert_eq!(fields[0].get("open"), Some(&2));
    assert_eq!(fields[0].get("class"), Some(&0));
}

#[test]
fn way_lines_areas_and_centroids() {
    let (mut p, shared) = profile(PROFILE);
    let road = p
        .process(Object {
            kind: ObjectKind::Way,
            id: 7,
            tags: tags(&[("highway", "primary")]),
            nodes: vec![ll(0.0, 0.0), ll(1.0, 0.0)],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(road.outputs.len(), 2);
    assert!(matches!(
        road.geometries[road.outputs[0].geometry],
        OutputGeometry::LineString(_)
    ));
    // Both outputs are lines of the same way: one geometry.
    assert_eq!(road.outputs[0].geometry, road.outputs[1].geometry);

    let lake = p
        .process(Object {
            kind: ObjectKind::Way,
            id: 8,
            tags: tags(&[("natural", "water")]),
            nodes: vec![ll(0.0, 0.0), ll(0.01, 0.0), ll(0.01, 0.01), ll(0.0, 0.01), ll(0.0, 0.0)],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(lake.outputs.len(), 2);
    let area = match &lake.outputs[0].attributes[0].value {
        AttributeValue::Float(a) => *a,
        other => panic!("{other:?}"),
    };
    // 0.01° × 0.01° at the equator ≈ 1.236 km².
    assert!((area - 1_236_000.0).abs() < 5_000.0, "{area}");
    match &lake.geometries[lake.outputs[1].geometry] {
        OutputGeometry::Point(pt) => {
            assert!((pt.lon_deg() - 0.005).abs() < 1e-4);
            assert!((pt.lat() - 0.005).abs() < 1e-4);
        }
        other => panic!("{other:?}"),
    }
    let lat: f64 = shared.data.lock().unwrap()["centroid_lat"].parse().unwrap();
    assert!((lat - 0.005).abs() < 1e-4);
}

#[test]
fn relations_scan_and_multipolygons() {
    let (mut p, _) = profile(PROFILE);
    assert!(p.scan_relation(1, tags(&[("type", "boundary")])).unwrap());
    assert!(!p.scan_relation(2, tags(&[("type", "route")])).unwrap());

    let ring = vec![ll(0.0, 0.0), ll(1.0, 0.0), ll(1.0, 1.0), ll(0.0, 1.0), ll(0.0, 0.0)];
    let hole = vec![ll(0.2, 0.2), ll(0.4, 0.2), ll(0.4, 0.4), ll(0.2, 0.2)];
    let mp = p
        .process(Object {
            kind: ObjectKind::Relation,
            id: 3,
            tags: tags(&[("type", "multipolygon"), ("natural", "wood")]),
            outers: vec![ring.clone()],
            inners: vec![hole],
            closed: true,
            multipolygon: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(mp.outputs.len(), 1);
    match &mp.geometries[0] {
        OutputGeometry::MultiPolygon(m) => {
            assert_eq!(m.len(), 1);
            assert_eq!(m.to_geo().0[0].interiors().len(), 1);
        }
        other => panic!("{other:?}"),
    }

    let b = p
        .process(Object {
            kind: ObjectKind::Relation,
            id: 4,
            tags: tags(&[
                ("type", "boundary"),
                ("boundary", "administrative"),
                ("admin_level", "2"),
            ]),
            outers: vec![ring[..3].to_vec(), ring[2..].to_vec()],
            closed: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(b.outputs[0].attributes[0].value, AttributeValue::String("2".into()));
    match &b.geometries[0] {
        OutputGeometry::MultiLineString(m) => assert_eq!(m.len(), 1, "joined"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn relation_membership_and_postscan() {
    let src = r#"
      function relation_scan_function() if Find("type") == "route" then Accept() end end
      function relation_postscan_function(rel)
        local parent = NextRelation()
        if parent then SetTag("network", FindInRelation("network")) end
      end
      function way_function()
        local names = {}
        while true do
          local rel, role = NextRelation()
          if not rel then break end
          table.insert(names, FindInRelation("ref") .. "/" .. role .. "/" .. FindInRelation("network"))
        end
        if #names > 0 then Layer("roads", false); Attribute("routes", table.concat(names, ";")) end
      end
    "#;
    let (mut p, _) = profile(src);
    let mut rels = ScannedRelations::new();
    for (id, t) in [
        (10, vec![("type", "route"), ("ref", "A1")]),
        (11, vec![("type", "route"), ("network", "e-road")]),
    ] {
        assert!(p.scan_relation(id, tags(&t)).unwrap());
        rels.store_tags(id, tags(&t));
    }
    rels.add_member(10, MemberKind::Way, 99, "forward");
    rels.add_member(11, MemberKind::Relation, 10, "");
    let ids = rels.relations_in_relations();
    assert_eq!(ids, vec![10]);
    let rels = p.postscan(rels, &ids).unwrap();
    assert_eq!(rels.tag(10, "network"), Some("e-road"));
    p.set_scanned_relations(Arc::new(rels.clone()));
    let out = p
        .process(Object {
            kind: ObjectKind::Way,
            id: 99,
            tags: vec![],
            nodes: vec![ll(0.0, 0.0), ll(1.0, 1.0)],
            relations: rels.relations_for(MemberKind::Way, 99),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        out.outputs[0].attributes[0].value,
        AttributeValue::String("A1/forward/e-road".into())
    );
}

#[test]
fn errors_name_the_object() {
    let (mut p, _) = profile("function node_function() Layer('nope') end");
    let e = p
        .process(Object {
            kind: ObjectKind::Node,
            id: 5,
            location: Some(ll(0.0, 0.0)),
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
    assert!(e.contains("node 5") && e.contains("nope"), "{e}");
    let config = Config::from_json(CONFIG).unwrap();
    assert!(LuaProfile::load("this is not lua", "bad.lua", Shared::new(&config), true).is_err());
}

#[test]
fn warnings_for_misuse() {
    let (mut p, shared) = profile("function node_function() Attribute('a', 'b'); MinZoom(3) end");
    let out = p
        .process(Object {
            kind: ObjectKind::Node,
            id: 1,
            location: Some(ll(0.0, 0.0)),
            ..Default::default()
        })
        .unwrap();
    assert!(out.is_empty());
    assert_eq!(shared.warnings.lock().unwrap().len(), 2);
}

#[test]
fn upstream_profiles_load() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources");
    for (lua, cfg) in [
        ("process-example.lua", "config-example.json"),
        ("process-openmaptiles.lua", "config-openmaptiles.json"),
    ] {
        let src = std::fs::read_to_string(dir.join(lua)).unwrap();
        let config = Config::from_path(dir.join(cfg)).unwrap();
        let p = LuaProfile::load(&src, lua, Shared::new(&config), true).unwrap_or_else(|e| panic!("{lua}: {e}"));
        assert!(p.capabilities().ways, "{lua}");
    }
}
