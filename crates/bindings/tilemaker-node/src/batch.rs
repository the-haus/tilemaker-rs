//! Columnar batches: records are packed into a few flat arrays on the
//! producer thread, so crossing into JS costs a handful of typed arrays and
//! one string per distinct string (tag keys repeat a lot), not one napi
//! call per coordinate. The TypeScript layer turns a batch back into
//! objects whose coordinates are zero-copy views into the batch's buffer.

use std::collections::HashMap;

use napi::bindgen_prelude::{Float64Array, Int32Array, Uint32Array, Uint8Array};
use napi::{Env, JsObject, Result};
use tilemaker_export::{Element, ElementType, Extras, Feature, GeometryType, Meta};

/// Per-batch string table.
#[derive(Default)]
pub(crate) struct Strings {
    list: Vec<String>,
    index: HashMap<String, u32>,
}

impl Strings {
    fn id(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.list.len() as u32;
        self.list.push(s.to_string());
        self.index.insert(s.to_string(), i);
        i
    }

    fn to_js(&self, env: &Env) -> Result<JsObject> {
        let mut arr = env.create_array_with_length(self.list.len())?;
        for (i, s) in self.list.iter().enumerate() {
            arr.set_element(i as u32, env.create_string(s)?)?;
        }
        Ok(arr)
    }
}

fn kind_code(k: ElementType) -> u8 {
    match k {
        ElementType::Node => 0,
        ElementType::Way => 1,
        ElementType::Relation => 2,
    }
}

fn geom_code(g: GeometryType) -> u8 {
    match g {
        GeometryType::Point => 0,
        GeometryType::LineString => 1,
        GeometryType::MultiLineString => 2,
        GeometryType::MultiPolygon => 3,
    }
}

/// Metadata columns.
#[derive(Default)]
struct MetaCols {
    present: Vec<u8>,
    version: Vec<i32>,
    timestamp: Vec<f64>,
    changeset: Vec<f64>,
    uid: Vec<i32>,
    user: Vec<u32>,
    visible: Vec<u8>,
}

impl MetaCols {
    fn push(&mut self, m: Option<&Meta>, strings: &mut Strings) {
        match m {
            Some(m) => {
                self.present.push(1);
                self.version.push(m.version);
                self.timestamp.push(m.timestamp as f64);
                self.changeset.push(m.changeset as f64);
                self.uid.push(m.uid);
                self.user.push(strings.id(&m.user));
                self.visible.push(m.visible as u8);
            }
            None => {
                self.present.push(0);
                self.version.push(0);
                self.timestamp.push(0.0);
                self.changeset.push(0.0);
                self.uid.push(0);
                self.user.push(0);
                self.visible.push(1);
            }
        }
    }

    fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        o.set("present", Uint8Array::new(self.present))?;
        o.set("version", Int32Array::new(self.version))?;
        o.set("timestamp", Float64Array::new(self.timestamp))?;
        o.set("changeset", Float64Array::new(self.changeset))?;
        o.set("uid", Int32Array::new(self.uid))?;
        o.set("user", Uint32Array::new(self.user))?;
        o.set("visible", Uint8Array::new(self.visible))?;
        Ok(o)
    }
}

/// Tag columns shared by both batch kinds.
#[derive(Default)]
struct TagCols {
    offsets: Vec<u32>,
    pairs: Vec<u32>,
}

impl TagCols {
    fn push(&mut self, tags: &[(String, String)], strings: &mut Strings) {
        if self.offsets.is_empty() {
            self.offsets.push(0);
        }
        for (k, v) in tags {
            self.pairs.push(strings.id(k));
            self.pairs.push(strings.id(v));
        }
        self.offsets.push(self.pairs.len() as u32);
    }
}

/// Parent relations: per feature a range of (relation, role), relations
/// listed once per batch with their tags.
#[derive(Default)]
struct MemberCols {
    offsets: Vec<u32>,
    relations: Vec<u32>,
    roles: Vec<u32>,
    index: HashMap<i64, u32>,
    ids: Vec<f64>,
    tags: TagCols,
}

/// Containing areas: per feature a range of areas, listed once per batch
/// with their tags.
#[derive(Default)]
struct AreaCols {
    offsets: Vec<u32>,
    areas: Vec<u32>,
    index: HashMap<(u8, i64), u32>,
    types: Vec<u8>,
    ids: Vec<f64>,
    tags: TagCols,
}

/// Computed values; a column exists when the stream computes it (the same
/// for every feature).
#[derive(Default)]
struct ExtraCols {
    area: Option<Vec<f64>>,
    length: Option<Vec<f64>>,
    centroid: Option<Vec<f64>>,
    label_point: Option<Vec<f64>>,
    bbox: Option<Vec<f64>>,
    members: Option<MemberCols>,
    within: Option<AreaCols>,
}

impl ExtraCols {
    fn push(&mut self, x: &Extras, strings: &mut Strings) {
        fn col<const N: usize>(c: &mut Option<Vec<f64>>, v: Option<[f64; N]>) {
            if let Some(v) = v {
                c.get_or_insert_with(Vec::new).extend_from_slice(&v);
            }
        }
        col(&mut self.area, x.area.map(|v| [v]));
        col(&mut self.length, x.length.map(|v| [v]));
        col(&mut self.centroid, x.centroid);
        col(&mut self.label_point, x.label_point);
        col(&mut self.bbox, x.bbox);
        if let Some(parents) = &x.member_of {
            let m = self.members.get_or_insert_with(|| MemberCols {
                offsets: vec![0],
                ..Default::default()
            });
            for p in parents {
                let rel = match m.index.get(&p.id) {
                    Some(&r) => r,
                    None => {
                        let r = m.ids.len() as u32;
                        m.ids.push(p.id as f64);
                        m.tags.push(&p.tags, strings);
                        m.index.insert(p.id, r);
                        r
                    }
                };
                m.relations.push(rel);
                m.roles.push(strings.id(&p.role));
            }
            m.offsets.push(m.relations.len() as u32);
        }
        if let Some(areas) = &x.within {
            let w = self.within.get_or_insert_with(|| AreaCols {
                offsets: vec![0],
                ..Default::default()
            });
            for a in areas {
                let key = (kind_code(a.kind), a.id);
                let i = match w.index.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = w.ids.len() as u32;
                        w.types.push(key.0);
                        w.ids.push(a.id as f64);
                        w.tags.push(&a.tags, strings);
                        w.index.insert(key, i);
                        i
                    }
                };
                w.areas.push(i);
            }
            w.offsets.push(w.areas.len() as u32);
        }
    }

    fn into_js(self, env: &Env, o: &mut JsObject) -> Result<()> {
        for (name, col) in [
            ("area", self.area),
            ("length", self.length),
            ("centroid", self.centroid),
            ("labelPoint", self.label_point),
            ("bbox", self.bbox),
        ] {
            if let Some(v) = col {
                o.set(name, Float64Array::new(v))?;
            }
        }
        if let Some(m) = self.members {
            let mut j = env.create_object()?;
            j.set("offsets", Uint32Array::new(m.offsets))?;
            j.set("relations", Uint32Array::new(m.relations))?;
            j.set("roles", Uint32Array::new(m.roles))?;
            j.set("ids", Float64Array::new(m.ids))?;
            j.set("tagOffsets", Uint32Array::new(m.tags.offsets))?;
            j.set("tags", Uint32Array::new(m.tags.pairs))?;
            o.set("memberOf", j)?;
        }
        if let Some(w) = self.within {
            let mut j = env.create_object()?;
            j.set("offsets", Uint32Array::new(w.offsets))?;
            j.set("areas", Uint32Array::new(w.areas))?;
            j.set("types", Uint8Array::new(w.types))?;
            j.set("ids", Float64Array::new(w.ids))?;
            j.set("tagOffsets", Uint32Array::new(w.tags.offsets))?;
            j.set("tags", Uint32Array::new(w.tags.pairs))?;
            o.set("within", j)?;
        }
        Ok(())
    }
}

/// A batch of features.
#[derive(Default)]
pub(crate) struct FeatureBatch {
    strings: Strings,
    kinds: Vec<u8>,
    ids: Vec<f64>,
    tags: TagCols,
    geom: Vec<u8>,
    coord_offsets: Vec<u32>,
    coords: Vec<f64>,
    part_offsets: Vec<u32>,
    parts: Vec<u32>,
    polygon_offsets: Vec<u32>,
    polygons: Vec<u32>,
    meta: Option<MetaCols>,
    extras: ExtraCols,
}

impl FeatureBatch {
    pub(crate) fn len(&self) -> usize {
        self.kinds.len()
    }

    pub(crate) fn coords_len(&self) -> usize {
        self.coords.len()
    }

    pub(crate) fn push(&mut self, f: Feature) {
        if self.coord_offsets.is_empty() {
            self.coord_offsets.push(0);
            self.part_offsets.push(0);
            self.polygon_offsets.push(0);
        }
        self.kinds.push(kind_code(f.kind));
        self.ids.push(f.id as f64);
        self.tags.push(&f.tags, &mut self.strings);
        self.geom.push(geom_code(f.geometry.kind));
        self.coords.extend_from_slice(&f.geometry.coords);
        self.coord_offsets.push(self.coords.len() as u32);
        self.parts.extend_from_slice(&f.geometry.parts);
        self.part_offsets.push(self.parts.len() as u32);
        self.polygons.extend_from_slice(&f.geometry.polygons);
        self.polygon_offsets.push(self.polygons.len() as u32);
        self.extras.push(&f.extras, &mut self.strings);
        if f.meta.is_some() || self.meta.is_some() {
            let n = self.kinds.len() - 1;
            let cols = self.meta.get_or_insert_with(|| {
                let mut c = MetaCols::default();
                for _ in 0..n {
                    c.push(None, &mut Strings::default());
                }
                c
            });
            cols.push(f.meta.as_ref(), &mut self.strings);
        }
    }

    pub(crate) fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        o.set("kind", "features")?;
        o.set("strings", self.strings.to_js(env)?)?;
        o.set("types", Uint8Array::new(self.kinds))?;
        o.set("ids", Float64Array::new(self.ids))?;
        o.set("tagOffsets", Uint32Array::new(self.tags.offsets))?;
        o.set("tags", Uint32Array::new(self.tags.pairs))?;
        o.set("geometryTypes", Uint8Array::new(self.geom))?;
        o.set("coordOffsets", Uint32Array::new(self.coord_offsets))?;
        o.set("coords", Float64Array::new(self.coords))?;
        o.set("partOffsets", Uint32Array::new(self.part_offsets))?;
        o.set("parts", Uint32Array::new(self.parts))?;
        o.set("polygonOffsets", Uint32Array::new(self.polygon_offsets))?;
        o.set("polygons", Uint32Array::new(self.polygons))?;
        self.extras.into_js(env, &mut o)?;
        if let Some(m) = self.meta {
            o.set("meta", m.into_js(env)?)?;
        }
        Ok(o)
    }
}

/// A batch of raw elements.
#[derive(Default)]
pub(crate) struct ElementBatch {
    strings: Strings,
    kinds: Vec<u8>,
    ids: Vec<f64>,
    tags: TagCols,
    /// Nodes: lon, lat; others: NaN, NaN.
    locations: Vec<f64>,
    ref_offsets: Vec<u32>,
    refs: Vec<f64>,
    /// Per way: whether `way_coords` holds its locations.
    has_way_coords: Vec<u8>,
    way_coord_offsets: Vec<u32>,
    way_coords: Vec<f64>,
    member_offsets: Vec<u32>,
    member_types: Vec<u8>,
    member_ids: Vec<f64>,
    member_roles: Vec<u32>,
    meta: Option<MetaCols>,
}

impl ElementBatch {
    pub(crate) fn len(&self) -> usize {
        self.kinds.len()
    }

    pub(crate) fn coords_len(&self) -> usize {
        self.way_coords.len() + self.refs.len()
    }

    pub(crate) fn push(&mut self, e: Element) {
        if self.ref_offsets.is_empty() {
            self.ref_offsets.push(0);
            self.way_coord_offsets.push(0);
            self.member_offsets.push(0);
        }
        self.kinds.push(kind_code(e.kind));
        self.ids.push(e.id as f64);
        self.tags.push(&e.tags, &mut self.strings);
        let [lon, lat] = e.location.unwrap_or([f64::NAN, f64::NAN]);
        self.locations.push(lon);
        self.locations.push(lat);
        self.refs.extend(e.refs.iter().map(|&r| r as f64));
        self.ref_offsets.push(self.refs.len() as u32);
        self.has_way_coords.push(e.locations.is_some() as u8);
        if let Some(l) = &e.locations {
            self.way_coords.extend_from_slice(l);
        }
        self.way_coord_offsets.push(self.way_coords.len() as u32);
        for m in &e.members {
            self.member_types.push(kind_code(m.kind));
            self.member_ids.push(m.id as f64);
            let r = self.strings.id(&m.role);
            self.member_roles.push(r);
        }
        self.member_offsets.push(self.member_ids.len() as u32);
        if e.meta.is_some() || self.meta.is_some() {
            let n = self.kinds.len() - 1;
            let cols = self.meta.get_or_insert_with(|| {
                let mut c = MetaCols::default();
                for _ in 0..n {
                    c.push(None, &mut Strings::default());
                }
                c
            });
            cols.push(e.meta.as_ref(), &mut self.strings);
        }
    }

    pub(crate) fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        o.set("kind", "elements")?;
        o.set("strings", self.strings.to_js(env)?)?;
        o.set("types", Uint8Array::new(self.kinds))?;
        o.set("ids", Float64Array::new(self.ids))?;
        o.set("tagOffsets", Uint32Array::new(self.tags.offsets))?;
        o.set("tags", Uint32Array::new(self.tags.pairs))?;
        o.set("locations", Float64Array::new(self.locations))?;
        o.set("refOffsets", Uint32Array::new(self.ref_offsets))?;
        o.set("refs", Float64Array::new(self.refs))?;
        o.set("hasWayCoords", Uint8Array::new(self.has_way_coords))?;
        o.set("wayCoordOffsets", Uint32Array::new(self.way_coord_offsets))?;
        o.set("wayCoords", Float64Array::new(self.way_coords))?;
        o.set("memberOffsets", Uint32Array::new(self.member_offsets))?;
        o.set("memberTypes", Uint8Array::new(self.member_types))?;
        o.set("memberIds", Float64Array::new(self.member_ids))?;
        o.set("memberRoles", Uint32Array::new(self.member_roles))?;
        if let Some(m) = self.meta {
            o.set("meta", m.into_js(env)?)?;
        }
        Ok(o)
    }
}
