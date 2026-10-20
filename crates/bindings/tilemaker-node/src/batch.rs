//! Columnar batches: records are packed into a few flat arrays on the
//! producer thread, so crossing into JS costs a handful of typed arrays and
//! one string per distinct string (tag keys repeat a lot), not one napi
//! call per coordinate. The TypeScript layer turns a batch back into
//! objects whose coordinates are zero-copy views into the batch's buffer.

use std::collections::HashMap;

use napi::{Env, JsObject, JsTypedArray, Result, TypedArrayType};
use tilemaker_export::{Element, ElementType, Extras, Feature, GeometryType, Meta};

/// A typed array in V8's own memory (a copy), so its garbage collector
/// counts it: external buffers made from Rust vectors are invisible to
/// it, and batches it has not collected yet would pile up natively.
fn typed<T: Copy>(env: &Env, v: &[T], kind: TypedArrayType) -> Result<JsTypedArray> {
    let bytes = std::mem::size_of_val(v);
    let mut buf = env.create_arraybuffer(bytes)?;
    // SAFETY: plain numbers, `bytes` long.
    let src = unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, bytes) };
    buf.as_mut().copy_from_slice(src);
    buf.into_raw().into_typedarray(kind, v.len(), 0)
}

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

    /// Merge `other`'s strings in; the new id of each of its ids.
    fn merge(&mut self, other: Strings) -> Vec<u32> {
        other.list.iter().map(|s| self.id(s)).collect()
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

/// Append `src` (offsets with a leading 0, or empty) to `dst`, shifted.
fn extend_offsets(dst: &mut Vec<u32>, src: &[u32], base: u32) {
    if src.is_empty() {
        return;
    }
    if dst.is_empty() {
        dst.push(0);
    }
    dst.extend(src[1..].iter().map(|&o| o + base));
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

    /// `n` absent entries.
    fn absent(n: usize) -> Self {
        let mut c = MetaCols::default();
        for _ in 0..n {
            c.push(None, &mut Strings::default());
        }
        c
    }

    fn append(&mut self, other: MetaCols, strings: &[u32]) {
        self.present.extend(other.present);
        self.version.extend(other.version);
        self.timestamp.extend(other.timestamp);
        self.changeset.extend(other.changeset);
        self.uid.extend(other.uid);
        self.user.extend(other.user.into_iter().map(|u| strings[u as usize]));
        self.visible.extend(other.visible);
    }

    fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        o.set("present", typed(env, &self.present, TypedArrayType::Uint8)?)?;
        o.set("version", typed(env, &self.version, TypedArrayType::Int32)?)?;
        o.set("timestamp", typed(env, &self.timestamp, TypedArrayType::Float64)?)?;
        o.set("changeset", typed(env, &self.changeset, TypedArrayType::Float64)?)?;
        o.set("uid", typed(env, &self.uid, TypedArrayType::Int32)?)?;
        o.set("user", typed(env, &self.user, TypedArrayType::Uint32)?)?;
        o.set("visible", typed(env, &self.visible, TypedArrayType::Uint8)?)?;
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
    fn append(&mut self, other: TagCols, strings: &[u32]) {
        let base = self.pairs.len() as u32;
        self.pairs.extend(other.pairs.iter().map(|&s| strings[s as usize]));
        extend_offsets(&mut self.offsets, &other.offsets, base);
    }

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
                o.set(name, typed(env, &v, TypedArrayType::Float64)?)?;
            }
        }
        if let Some(m) = self.members {
            let mut j = env.create_object()?;
            j.set("offsets", typed(env, &m.offsets, TypedArrayType::Uint32)?)?;
            j.set("relations", typed(env, &m.relations, TypedArrayType::Uint32)?)?;
            j.set("roles", typed(env, &m.roles, TypedArrayType::Uint32)?)?;
            j.set("ids", typed(env, &m.ids, TypedArrayType::Float64)?)?;
            j.set("tagOffsets", typed(env, &m.tags.offsets, TypedArrayType::Uint32)?)?;
            j.set("tags", typed(env, &m.tags.pairs, TypedArrayType::Uint32)?)?;
            o.set("memberOf", j)?;
        }
        if let Some(w) = self.within {
            let mut j = env.create_object()?;
            j.set("offsets", typed(env, &w.offsets, TypedArrayType::Uint32)?)?;
            j.set("areas", typed(env, &w.areas, TypedArrayType::Uint32)?)?;
            j.set("types", typed(env, &w.types, TypedArrayType::Uint8)?)?;
            j.set("ids", typed(env, &w.ids, TypedArrayType::Float64)?)?;
            j.set("tagOffsets", typed(env, &w.tags.offsets, TypedArrayType::Uint32)?)?;
            j.set("tags", typed(env, &w.tags.pairs, TypedArrayType::Uint32)?)?;
            o.set("within", j)?;
        }
        Ok(())
    }
}

/// Append metadata columns of a batch of `other_len` records to those of
/// one of `len` records (either may have none).
fn append_meta(dst: &mut Option<MetaCols>, len: usize, src: Option<MetaCols>, other_len: usize, strings: &[u32]) {
    match (dst.as_mut(), src) {
        (Some(d), None) => d.append(MetaCols::absent(other_len), strings),
        (Some(d), Some(o)) => d.append(o, strings),
        (None, Some(o)) => {
            let mut d = MetaCols::absent(len);
            d.append(o, strings);
            *dst = Some(d);
        }
        (None, None) => {}
    }
}

impl MemberCols {
    fn append(&mut self, o: MemberCols, strings: &[u32]) {
        let rels: Vec<u32> = (0..o.ids.len())
            .map(|r| {
                let id = o.ids[r] as i64;
                *self.index.entry(id).or_insert_with(|| {
                    let i = self.ids.len() as u32;
                    self.ids.push(o.ids[r]);
                    let (a, b) = (o.tags.offsets[r] as usize, o.tags.offsets[r + 1] as usize);
                    let base = self.tags.pairs.len() as u32;
                    self.tags
                        .pairs
                        .extend(o.tags.pairs[a..b].iter().map(|&s| strings[s as usize]));
                    extend_offsets(&mut self.tags.offsets, &[0, (b - a) as u32], base);
                    i
                })
            })
            .collect();
        let base = self.relations.len() as u32;
        self.relations.extend(o.relations.iter().map(|&r| rels[r as usize]));
        self.roles.extend(o.roles.iter().map(|&s| strings[s as usize]));
        extend_offsets(&mut self.offsets, &o.offsets, base);
    }
}

impl AreaCols {
    fn append(&mut self, o: AreaCols, strings: &[u32]) {
        let areas: Vec<u32> = (0..o.ids.len())
            .map(|a| {
                let key = (o.types[a], o.ids[a] as i64);
                *self.index.entry(key).or_insert_with(|| {
                    let i = self.ids.len() as u32;
                    self.types.push(o.types[a]);
                    self.ids.push(o.ids[a]);
                    let (s0, s1) = (o.tags.offsets[a] as usize, o.tags.offsets[a + 1] as usize);
                    let base = self.tags.pairs.len() as u32;
                    self.tags
                        .pairs
                        .extend(o.tags.pairs[s0..s1].iter().map(|&s| strings[s as usize]));
                    extend_offsets(&mut self.tags.offsets, &[0, (s1 - s0) as u32], base);
                    i
                })
            })
            .collect();
        let base = self.areas.len() as u32;
        self.areas.extend(o.areas.iter().map(|&a| areas[a as usize]));
        extend_offsets(&mut self.offsets, &o.offsets, base);
    }
}

impl ExtraCols {
    fn append(&mut self, o: ExtraCols, strings: &[u32]) {
        for (d, s) in [
            (&mut self.area, o.area),
            (&mut self.length, o.length),
            (&mut self.centroid, o.centroid),
            (&mut self.label_point, o.label_point),
            (&mut self.bbox, o.bbox),
        ] {
            if let Some(s) = s {
                d.get_or_insert_with(Vec::new).extend(s);
            }
        }
        if let Some(m) = o.members {
            self.members.get_or_insert_with(Default::default).append(m, strings);
        }
        if let Some(w) = o.within {
            self.within.get_or_insert_with(Default::default).append(w, strings);
        }
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

    /// Move `other`'s features to the end of this batch.
    pub(crate) fn append(&mut self, o: FeatureBatch) {
        if self.len() == 0 {
            *self = o;
            return;
        }
        let (len, olen) = (self.len(), o.len());
        let strings = self.strings.merge(o.strings);
        self.kinds.extend(o.kinds);
        self.ids.extend(o.ids);
        self.tags.append(o.tags, &strings);
        self.geom.extend(o.geom);
        let base = self.coords.len() as u32;
        self.coords.extend(o.coords);
        extend_offsets(&mut self.coord_offsets, &o.coord_offsets, base);
        let base = self.parts.len() as u32;
        self.parts.extend(o.parts);
        extend_offsets(&mut self.part_offsets, &o.part_offsets, base);
        let base = self.polygons.len() as u32;
        self.polygons.extend(o.polygons);
        extend_offsets(&mut self.polygon_offsets, &o.polygon_offsets, base);
        append_meta(&mut self.meta, len, o.meta, olen, &strings);
        self.extras.append(o.extras, &strings);
    }

    pub(crate) fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        o.set("kind", "features")?;
        o.set("strings", self.strings.to_js(env)?)?;
        o.set("types", typed(env, &self.kinds, TypedArrayType::Uint8)?)?;
        o.set("ids", typed(env, &self.ids, TypedArrayType::Float64)?)?;
        o.set("tagOffsets", typed(env, &self.tags.offsets, TypedArrayType::Uint32)?)?;
        o.set("tags", typed(env, &self.tags.pairs, TypedArrayType::Uint32)?)?;
        o.set("geometryTypes", typed(env, &self.geom, TypedArrayType::Uint8)?)?;
        o.set("coordOffsets", typed(env, &self.coord_offsets, TypedArrayType::Uint32)?)?;
        o.set("coords", typed(env, &self.coords, TypedArrayType::Float64)?)?;
        o.set("partOffsets", typed(env, &self.part_offsets, TypedArrayType::Uint32)?)?;
        o.set("parts", typed(env, &self.parts, TypedArrayType::Uint32)?)?;
        o.set(
            "polygonOffsets",
            typed(env, &self.polygon_offsets, TypedArrayType::Uint32)?,
        )?;
        o.set("polygons", typed(env, &self.polygons, TypedArrayType::Uint32)?)?;
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

    /// Move `other`'s elements to the end of this batch.
    pub(crate) fn append(&mut self, o: ElementBatch) {
        if self.len() == 0 {
            *self = o;
            return;
        }
        let (len, olen) = (self.len(), o.len());
        let strings = self.strings.merge(o.strings);
        self.kinds.extend(o.kinds);
        self.ids.extend(o.ids);
        self.tags.append(o.tags, &strings);
        self.locations.extend(o.locations);
        let base = self.refs.len() as u32;
        self.refs.extend(o.refs);
        extend_offsets(&mut self.ref_offsets, &o.ref_offsets, base);
        self.has_way_coords.extend(o.has_way_coords);
        let base = self.way_coords.len() as u32;
        self.way_coords.extend(o.way_coords);
        extend_offsets(&mut self.way_coord_offsets, &o.way_coord_offsets, base);
        let base = self.member_ids.len() as u32;
        self.member_types.extend(o.member_types);
        self.member_ids.extend(o.member_ids);
        self.member_roles
            .extend(o.member_roles.iter().map(|&r| strings[r as usize]));
        extend_offsets(&mut self.member_offsets, &o.member_offsets, base);
        append_meta(&mut self.meta, len, o.meta, olen, &strings);
    }

    pub(crate) fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        o.set("kind", "elements")?;
        o.set("strings", self.strings.to_js(env)?)?;
        o.set("types", typed(env, &self.kinds, TypedArrayType::Uint8)?)?;
        o.set("ids", typed(env, &self.ids, TypedArrayType::Float64)?)?;
        o.set("tagOffsets", typed(env, &self.tags.offsets, TypedArrayType::Uint32)?)?;
        o.set("tags", typed(env, &self.tags.pairs, TypedArrayType::Uint32)?)?;
        o.set("locations", typed(env, &self.locations, TypedArrayType::Float64)?)?;
        o.set("refOffsets", typed(env, &self.ref_offsets, TypedArrayType::Uint32)?)?;
        o.set("refs", typed(env, &self.refs, TypedArrayType::Float64)?)?;
        o.set("hasWayCoords", typed(env, &self.has_way_coords, TypedArrayType::Uint8)?)?;
        o.set(
            "wayCoordOffsets",
            typed(env, &self.way_coord_offsets, TypedArrayType::Uint32)?,
        )?;
        o.set("wayCoords", typed(env, &self.way_coords, TypedArrayType::Float64)?)?;
        o.set(
            "memberOffsets",
            typed(env, &self.member_offsets, TypedArrayType::Uint32)?,
        )?;
        o.set("memberTypes", typed(env, &self.member_types, TypedArrayType::Uint8)?)?;
        o.set("memberIds", typed(env, &self.member_ids, TypedArrayType::Float64)?)?;
        o.set("memberRoles", typed(env, &self.member_roles, TypedArrayType::Uint32)?)?;
        if let Some(m) = self.meta {
            o.set("meta", m.into_js(env)?)?;
        }
        Ok(o)
    }
}
