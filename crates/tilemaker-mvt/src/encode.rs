//! Vector tile encoder.

use std::collections::HashMap;

use crate::pbf::{
    write_bytes_field, write_packed_u32, write_tag, write_varint, write_varint_field, zigzag32, zigzag64, WIRE_FIXED32,
    WIRE_FIXED64,
};
use crate::{command, DecodedFeature, DecodedLayer, GeomType, Value, CMD_CLOSE_PATH, CMD_LINE_TO, CMD_MOVE_TO};

/// Builds a complete vector tile out of named layers.
#[derive(Debug, Default)]
pub struct TileBuilder {
    layers: Vec<LayerBuilder>,
    index: HashMap<String, usize>,
}

impl TileBuilder {
    /// Creates an empty tile.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the layer called `name`, creating it (appended after any
    /// existing layers) if necessary. `version` and `extent` are only used
    /// when the layer is created.
    pub fn layer(&mut self, name: &str, version: u32, extent: u32) -> &mut LayerBuilder {
        let idx = match self.index.get(name) {
            Some(&i) => i,
            None => {
                self.layers.push(LayerBuilder::new(name, version, extent));
                self.index.insert(name.to_string(), self.layers.len() - 1);
                self.layers.len() - 1
            }
        };
        &mut self.layers[idx]
    }

    /// Returns true when no layer contains any feature.
    pub fn is_empty(&self) -> bool {
        self.layers.iter().all(|l| l.count == 0)
    }

    /// Iterates over the layers in insertion order.
    pub fn layers(&self) -> impl Iterator<Item = &LayerBuilder> {
        self.layers.iter()
    }

    /// Serialises the tile. Layers without features are omitted.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut layer_buf = Vec::new();
        for layer in self.layers.iter().filter(|l| l.count > 0) {
            layer_buf.clear();
            layer.encode_into(&mut layer_buf);
            write_bytes_field(&mut out, 3, &layer_buf);
        }
        out
    }
}

/// Key used to deduplicate property values: kind plus exact bit pattern.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ValueKey {
    Str(String),
    Num(u8, u64),
}

impl ValueKey {
    fn of(v: &Value) -> ValueKey {
        match v {
            Value::String(s) => ValueKey::Str(s.clone()),
            Value::Float(f) => ValueKey::Num(2, u64::from(f.to_bits())),
            Value::Double(d) => ValueKey::Num(3, d.to_bits()),
            Value::Int(i) => ValueKey::Num(4, *i as u64),
            Value::UInt(u) => ValueKey::Num(5, *u),
            Value::SInt(i) => ValueKey::Num(6, *i as u64),
            Value::Bool(b) => ValueKey::Num(7, u64::from(*b)),
        }
    }
}

/// One layer of a tile under construction.
#[derive(Debug)]
pub struct LayerBuilder {
    name: String,
    version: u32,
    extent: u32,
    keys: Vec<String>,
    key_index: HashMap<String, u32>,
    values: Vec<Value>,
    value_index: HashMap<ValueKey, u32>,
    /// Encoded `features` fields (field 2 of the layer message), concatenated.
    features: Vec<u8>,
    count: usize,
}

impl LayerBuilder {
    fn new(name: &str, version: u32, extent: u32) -> Self {
        LayerBuilder {
            name: name.to_string(),
            version,
            extent,
            keys: Vec::new(),
            key_index: HashMap::new(),
            values: Vec::new(),
            value_index: HashMap::new(),
            features: Vec::new(),
            count: 0,
        }
    }

    /// Starts a new feature of the given geometry type. The feature is only
    /// added to the layer when [`FeatureBuilder::commit`] is called.
    pub fn feature(&mut self, geom_type: GeomType) -> FeatureBuilder<'_> {
        FeatureBuilder {
            layer: self,
            geom_type,
            id: None,
            tags: Vec::new(),
            geometry: Vec::new(),
            cursor: (0, 0),
        }
    }

    /// Number of committed features.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Returns true if no feature has been committed.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Layer name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Layer extent.
    pub fn extent(&self) -> u32 {
        self.extent
    }

    /// Copies a decoded feature (from `src_layer`) into this layer, keeping
    /// its id, properties and raw geometry. Returns whether it was added.
    pub fn add_decoded_feature(&mut self, feature: &DecodedFeature, src_layer: &DecodedLayer) -> bool {
        let mut fb = self.feature(feature.geom_type);
        if let Some(id) = feature.id {
            fb.set_id(id);
        }
        for (k, v) in feature.properties(src_layer) {
            fb.add_property(k, v.clone());
        }
        fb.add_raw_geometry(&feature.geometry);
        fb.commit()
    }

    fn key_id(&mut self, key: &str) -> u32 {
        if let Some(&i) = self.key_index.get(key) {
            return i;
        }
        let i = self.keys.len() as u32;
        self.keys.push(key.to_string());
        self.key_index.insert(key.to_string(), i);
        i
    }

    fn value_id(&mut self, value: Value) -> u32 {
        let key = ValueKey::of(&value);
        if let Some(&i) = self.value_index.get(&key) {
            return i;
        }
        let i = self.values.len() as u32;
        self.values.push(value);
        self.value_index.insert(key, i);
        i
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        write_varint_field(out, 15, u64::from(self.version));
        write_bytes_field(out, 1, self.name.as_bytes());
        out.extend_from_slice(&self.features);
        for k in &self.keys {
            write_bytes_field(out, 3, k.as_bytes());
        }
        let mut vbuf = Vec::new();
        for v in &self.values {
            vbuf.clear();
            encode_value(&mut vbuf, v);
            write_bytes_field(out, 4, &vbuf);
        }
        write_varint_field(out, 5, u64::from(self.extent));
    }
}

fn encode_value(buf: &mut Vec<u8>, v: &Value) {
    match v {
        Value::String(s) => write_bytes_field(buf, 1, s.as_bytes()),
        Value::Float(f) => {
            write_tag(buf, 2, WIRE_FIXED32);
            buf.extend_from_slice(&f.to_le_bytes());
        }
        Value::Double(d) => {
            write_tag(buf, 3, WIRE_FIXED64);
            buf.extend_from_slice(&d.to_le_bytes());
        }
        Value::Int(i) => write_varint_field(buf, 4, *i as u64),
        Value::UInt(u) => write_varint_field(buf, 5, *u),
        Value::SInt(i) => write_varint_field(buf, 6, zigzag64(*i)),
        Value::Bool(b) => write_varint_field(buf, 7, u64::from(*b)),
    }
}

/// A feature under construction. Call [`commit`](FeatureBuilder::commit) to
/// add it to its layer; dropping it discards the feature (any keys/values it
/// interned remain in the layer's tables, which is harmless).
#[derive(Debug)]
pub struct FeatureBuilder<'a> {
    layer: &'a mut LayerBuilder,
    geom_type: GeomType,
    id: Option<u64>,
    tags: Vec<u32>,
    geometry: Vec<u32>,
    cursor: (i32, i32),
}

impl FeatureBuilder<'_> {
    /// Sets the feature id.
    pub fn set_id(&mut self, id: u64) {
        self.id = Some(id);
    }

    /// Adds a property; keys and values are deduplicated per layer.
    pub fn add_property(&mut self, key: &str, value: Value) {
        let k = self.layer.key_id(key);
        let v = self.layer.value_id(value);
        self.tags.push(k);
        self.tags.push(v);
    }

    fn push_point(&mut self, p: (i32, i32)) {
        let dx = p.0.wrapping_sub(self.cursor.0);
        let dy = p.1.wrapping_sub(self.cursor.1);
        self.geometry.push(zigzag32(dx));
        self.geometry.push(zigzag32(dy));
        self.cursor = p;
    }

    /// Adds points as a single MoveTo command with count `pts.len()`.
    pub fn add_points(&mut self, pts: &[(i32, i32)]) {
        if pts.is_empty() {
            return;
        }
        self.geometry.push(command(CMD_MOVE_TO, pts.len() as u32));
        for &p in pts {
            self.push_point(p);
        }
    }

    /// Adds a linestring (MoveTo + LineTo). Zero-length segments are skipped,
    /// and the line is dropped if fewer than two distinct points remain.
    pub fn add_linestring(&mut self, pts: &[(i32, i32)]) {
        let pts = dedup_consecutive(pts);
        if pts.len() < 2 {
            return;
        }
        self.geometry.push(command(CMD_MOVE_TO, 1));
        self.push_point(pts[0]);
        self.geometry.push(command(CMD_LINE_TO, (pts.len() - 1) as u32));
        for &p in &pts[1..] {
            self.push_point(p);
        }
    }

    /// Adds a polygon ring, given closed (first == last) or open. Writes
    /// MoveTo, LineTo and ClosePath; zero-length segments are skipped and the
    /// ring is dropped if it has fewer than three distinct points. Point
    /// order is preserved (the caller is responsible for winding).
    pub fn add_ring(&mut self, pts: &[(i32, i32)]) {
        let mut pts = dedup_consecutive(pts);
        while pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        if pts.len() < 3 {
            return;
        }
        self.geometry.push(command(CMD_MOVE_TO, 1));
        self.push_point(pts[0]);
        self.geometry.push(command(CMD_LINE_TO, (pts.len() - 1) as u32));
        for &p in &pts[1..] {
            self.push_point(p);
        }
        self.geometry.push(command(CMD_CLOSE_PATH, 1));
    }

    /// Appends already-encoded geometry commands (e.g. a decoded feature's
    /// complete geometry). Deltas are relative to the current cursor, so this
    /// is normally the only geometry call on a feature; the cursor is
    /// advanced past the commands so later calls remain consistent.
    pub fn add_raw_geometry(&mut self, cmds: &[u32]) {
        self.geometry.extend_from_slice(cmds);
        let mut i = 0;
        while i < cmds.len() {
            let id = cmds[i] & 7;
            let count = (cmds[i] >> 3) as usize;
            i += 1;
            if id == CMD_MOVE_TO || id == CMD_LINE_TO {
                for _ in 0..count {
                    if i + 1 >= cmds.len() {
                        return;
                    }
                    self.cursor.0 = self.cursor.0.wrapping_add(crate::pbf::unzigzag32(cmds[i]));
                    self.cursor.1 = self.cursor.1.wrapping_add(crate::pbf::unzigzag32(cmds[i + 1]));
                    i += 2;
                }
            }
        }
    }

    /// Returns true if any geometry has been added.
    pub fn has_geometry(&self) -> bool {
        !self.geometry.is_empty()
    }

    /// Appends the feature to its layer if it has geometry; returns whether
    /// it was appended.
    pub fn commit(self) -> bool {
        if self.geometry.is_empty() {
            return false;
        }
        let mut f = Vec::with_capacity(self.geometry.len() * 2 + self.tags.len() + 8);
        if let Some(id) = self.id {
            write_varint_field(&mut f, 1, id);
        }
        write_packed_u32(&mut f, 2, &self.tags);
        write_varint_field(&mut f, 3, self.geom_type as u64);
        write_packed_u32(&mut f, 4, &self.geometry);

        let out = &mut self.layer.features;
        write_tag(out, 2, crate::pbf::WIRE_LEN);
        write_varint(out, f.len() as u64);
        out.extend_from_slice(&f);
        self.layer.count += 1;
        true
    }
}

fn dedup_consecutive(pts: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = Vec::with_capacity(pts.len());
    for &p in pts {
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}
