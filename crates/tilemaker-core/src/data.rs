//! Everything read from the OSM file that tiles are made of: output
//! objects, their geometries and attribute sets, and the tile index.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use rustc_hash::{FxHashMap, FxHasher};

use tilemaker_geom::compact::to_coords;
use tilemaker_geom::coords::{latp2tileyf, lon2tilexf, TileCoord, MAX_LATP, MIN_LATP};
use tilemaker_geom::tiles::{fill_covered_tiles, insert_intermediate_tiles};
use tilemaker_geom::{ll_to_coord, Coord, LatpLon};
use tilemaker_lua::{Attribute, AttributeValue, GeometryKind, OutputGeometry, PendingOutput, ProcessedObject};

/// One `Layer()` output, ready for tiles (16 bytes).
#[derive(Clone, Copy, Debug)]
pub struct OutputObject {
    /// Layer index.
    pub layer: u8,
    /// Geometry kind.
    pub kind: GeometryKind,
    /// Minimum zoom.
    pub min_zoom: u8,
    /// `ZOrder()`.
    pub z_order: f32,
    /// Attribute set index.
    pub attributes: u32,
    /// Geometry index.
    pub geometry: u32,
}

/// Where an object goes in the index.
#[derive(Clone, Debug)]
pub enum Coverage {
    /// These base-zoom tiles.
    Tiles(Vec<TileCoord>),
    /// Every base-zoom tile in `[min, max]` (large polygons).
    Rect(TileCoord, TileCoord),
}

/// A geometry packed for the store: a point, or the end of its record.
#[derive(Clone, Copy, Debug)]
pub enum Packed {
    /// A point.
    Point(LatpLon),
    /// The exclusive end of its record in [`IndexedObject::records`].
    Record(u32),
}

/// A processed object with its coverage and packed geometries, as produced
/// by a worker.
pub struct IndexedObject {
    /// The object's id.
    pub id: i64,
    /// The profile's outputs.
    pub outputs: Vec<PendingOutput>,
    /// Per geometry: its kind and where it is.
    pub geometries: Vec<(GeometryKind, Packed)>,
    /// The geometries' records, back to back.
    pub records: Vec<u8>,
    /// Coverage per geometry.
    pub coverage: Vec<Coverage>,
}

/// Pack an object's geometries (see [`tilemaker_geom::packed`]).
fn pack(geometries: &[OutputGeometry]) -> (Vec<(GeometryKind, Packed)>, Vec<u8>) {
    use tilemaker_geom::packed;
    let mut records = Vec::new();
    let out = geometries
        .iter()
        .map(|g| {
            match g {
                OutputGeometry::Point(p) => return (g.kind(), Packed::Point(*p)),
                OutputGeometry::LineString(l) => packed::encode_line(l, &mut records),
                OutputGeometry::MultiLineString(l) => packed::encode_lines(l, &mut records),
                OutputGeometry::MultiPolygon(p) => packed::encode_polygons(p, &mut records),
            }
            (g.kind(), Packed::Record(records.len() as u32))
        })
        .collect();
    (out, records)
}

/// Polygons covering this many base tiles or more go to the rectangle list
/// rather than into every tile (as tilemaker's "large index").
const LARGE: u64 = 16;

/// What an object covers, for points it labels: tilemaker indexes a
/// `LayerAsCentroid` point of a way or relation in every tile the way or
/// relation touches (each tile then keeps it if it falls within the tile's
/// buffer), so labels near tile edges also reach the neighbouring tiles.
pub enum Footprint<'a> {
    /// A node: points cover their own tile.
    Node,
    /// A way's node locations.
    Way(&'a [LatpLon]),
    /// A relation's member ways.
    Relation(&'a [Vec<LatpLon>]),
}

/// Compute which base-zoom tiles each geometry of an object touches.
pub fn coverage(object: ProcessedObject, base_zoom: u8, footprint: Footprint<'_>) -> IndexedObject {
    let mut labels: Option<Coverage> = None;
    let coverage = object
        .geometries
        .iter()
        .map(|g| match (g, &footprint) {
            (OutputGeometry::Point(_), Footprint::Way(nodes)) => labels
                .get_or_insert_with(|| {
                    let mut set = HashSet::new();
                    let line: Vec<Coord> = nodes.iter().map(|&n| ll_to_coord(n)).collect();
                    insert_intermediate_tiles(&line, base_zoom, &mut set);
                    let mut v: Vec<TileCoord> = set.into_iter().collect();
                    v.sort_unstable();
                    Coverage::Tiles(v)
                })
                .clone(),
            (OutputGeometry::Point(_), Footprint::Relation(ways)) => labels
                .get_or_insert_with(|| {
                    let mut set = HashSet::new();
                    for w in ways.iter() {
                        let line: Vec<Coord> = w.iter().map(|&n| ll_to_coord(n)).collect();
                        insert_intermediate_tiles(&line, base_zoom, &mut set);
                    }
                    if set.is_empty() {
                        return Coverage::Tiles(Vec::new());
                    }
                    let (min, max) = bounds(&set);
                    if (max.x - min.x + 1) as u64 * (max.y - min.y + 1) as u64 >= LARGE {
                        return Coverage::Rect(min, max);
                    }
                    fill_covered_tiles(&mut set);
                    let mut v: Vec<TileCoord> = set.into_iter().collect();
                    v.sort_unstable();
                    Coverage::Tiles(v)
                })
                .clone(),
            _ => cover(g, base_zoom),
        })
        .collect();
    let (geometries, records) = pack(&object.geometries);
    IndexedObject {
        id: object.id,
        outputs: object.outputs,
        geometries,
        records,
        coverage,
    }
}

fn cover(g: &OutputGeometry, z: u8) -> Coverage {
    let mut set = HashSet::new();
    match g {
        OutputGeometry::Point(p) => {
            let c = ll_to_coord(*p);
            return Coverage::Tiles(vec![tile_of(c, z)]);
        }
        OutputGeometry::LineString(ls) => insert_intermediate_tiles(&to_coords(ls), z, &mut set),
        OutputGeometry::MultiLineString(mls) => {
            for ls in mls.lines() {
                insert_intermediate_tiles(&to_coords(ls), z, &mut set);
            }
        }
        OutputGeometry::MultiPolygon(mp) => {
            let exteriors: Vec<Vec<Coord>> = mp.exteriors().map(to_coords).collect();
            let mut all = HashSet::new();
            for ext in &exteriors {
                let mut s = HashSet::new();
                insert_intermediate_tiles(ext, z, &mut s);
                let (min, max) = bounds(&s);
                if (max.x - min.x + 1) as u64 * (max.y - min.y + 1) as u64 >= LARGE {
                    // Large: index by rectangle, all parts together.
                    let (mut min, mut max) = (min, max);
                    for other in &exteriors {
                        let mut t = HashSet::new();
                        insert_intermediate_tiles(other, z, &mut t);
                        let (a, b) = bounds(&t);
                        min = TileCoord::new(min.x.min(a.x), min.y.min(a.y));
                        max = TileCoord::new(max.x.max(b.x), max.y.max(b.y));
                    }
                    return Coverage::Rect(min, max);
                }
                fill_covered_tiles(&mut s);
                all.extend(s);
            }
            set = all;
        }
    }
    let mut v: Vec<TileCoord> = set.into_iter().collect();
    v.sort_unstable();
    Coverage::Tiles(v)
}

fn bounds(s: &HashSet<TileCoord>) -> (TileCoord, TileCoord) {
    let mut min = TileCoord::new(u32::MAX, u32::MAX);
    let mut max = TileCoord::new(0, 0);
    for t in s {
        min = TileCoord::new(min.x.min(t.x), min.y.min(t.y));
        max = TileCoord::new(max.x.max(t.x), max.y.max(t.y));
    }
    (min, max)
}

fn tile_of(c: Coord, z: u8) -> TileCoord {
    let max = ((1u64 << z) - 1) as f64;
    TileCoord::new(
        lon2tilexf(c.x, z).clamp(0.0, max) as u32,
        latp2tileyf(c.y.clamp(MIN_LATP, MAX_LATP), z).clamp(0.0, max) as u32,
    )
}

/// Output geometries without an allocation each: a point is kept in its
/// 8-byte slot, anything else as a packed record in large pages (see
/// [`tilemaker_geom::packed`]).
#[derive(Default)]
pub struct GeometryStore {
    /// Per geometry: a point's `lon << 32 | latp`, or where its record
    /// starts (`page << 32 | offset`).
    slots: Vec<u64>,
    pages: Vec<Vec<u8>>,
}

/// Bytes per page (reserved; only what is written is touched).
const PAGE: usize = 64 << 20;

impl GeometryStore {
    /// Number of geometries.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    fn push_point(&mut self, p: LatpLon) -> u32 {
        self.slots.push((p.lon as u32 as u64) << 32 | p.latp as u32 as u64);
        self.slots.len() as u32 - 1
    }

    fn push_record(&mut self, rec: &[u8]) -> u32 {
        if self.pages.last().is_none_or(|p| p.len() + rec.len() > p.capacity()) {
            if let Some(p) = self.pages.last_mut() {
                p.shrink_to_fit();
            }
            self.pages.push(Vec::with_capacity(PAGE.max(rec.len())));
        }
        let page = self.pages.len() - 1;
        let p = &mut self.pages[page];
        self.slots.push((page as u64) << 32 | p.len() as u64);
        p.extend_from_slice(rec);
        self.slots.len() as u32 - 1
    }

    /// Geometry `i`, a point.
    #[inline]
    pub fn point(&self, i: u32) -> LatpLon {
        let s = self.slots[i as usize];
        LatpLon {
            lon: (s >> 32) as u32 as i32,
            latp: s as u32 as i32,
        }
    }

    /// Geometry `i`'s record (and whatever follows it).
    #[inline]
    pub fn record(&self, i: u32) -> &[u8] {
        let s = self.slots[i as usize];
        &self.pages[(s >> 32) as usize][(s as u32) as usize..]
    }

    /// Heap bytes in use.
    pub fn bytes(&self) -> usize {
        self.slots.capacity() * 8 + self.pages.iter().map(|p| p.len()).sum::<usize>()
    }

    fn shrink(&mut self) {
        self.slots.shrink_to_fit();
        if let Some(p) = self.pages.last_mut() {
            p.shrink_to_fit();
        }
    }
}

/// The tile data.
#[derive(Default)]
pub struct TileData {
    /// Base zoom of the index.
    pub base_zoom: u8,
    /// Output objects.
    pub objects: Vec<OutputObject>,
    /// Geometries.
    pub geometries: GeometryStore,
    /// Per object, its OSM id (only with `include_ids`).
    pub ids: Vec<i64>,
    include_ids: bool,
    /// Distinct attribute sets.
    pub attributes: AttributeStore,
    /// Base-zoom tile → objects.
    pub tiles: HashMap<TileCoord, Vec<u32>>,
    /// Large objects: base-zoom tile rectangle → object.
    pub large: Vec<(TileCoord, TileCoord, u32)>,
}

impl TileData {
    /// Empty data indexed at `base_zoom`; `include_ids` keeps objects' ids.
    pub fn new(base_zoom: u8, include_ids: bool) -> Self {
        Self {
            base_zoom,
            include_ids,
            ..Default::default()
        }
    }

    /// Add one indexed object.
    pub fn add(&mut self, o: IndexedObject) {
        let IndexedObject {
            id,
            outputs,
            geometries,
            records,
            coverage,
        } = o;
        let base = self.geometries.len() as u32;
        let mut start = 0usize;
        for &(_, g) in &geometries {
            match g {
                Packed::Point(p) => self.geometries.push_point(p),
                Packed::Record(end) => {
                    let i = self.geometries.push_record(&records[start..end as usize]);
                    start = end as usize;
                    i
                }
            };
        }
        for out in outputs {
            if out.layer == u8::MAX {
                continue;
            }
            let attributes = self.attributes.insert(out.attributes);
            let geometry = base + out.geometry as u32;
            let obj = self.objects.len() as u32;
            self.objects.push(OutputObject {
                layer: out.layer,
                kind: geometries[out.geometry].0,
                min_zoom: out.min_zoom,
                z_order: out.z_order,
                attributes,
                geometry,
            });
            if self.include_ids {
                self.ids.push(id);
            }
            match &coverage[out.geometry] {
                Coverage::Tiles(tiles) => {
                    for t in tiles {
                        self.tiles.entry(*t).or_default().push(obj);
                    }
                }
                Coverage::Rect(a, b) => self.large.push((*a, *b, obj)),
            }
        }
    }

    /// Release memory only needed while adding.
    pub fn seal(&mut self) {
        self.attributes.seal();
        self.ids.shrink_to_fit();
        self.objects.shrink_to_fit();
        self.geometries.shrink();
        for v in self.tiles.values_mut() {
            v.shrink_to_fit();
        }
    }
}

/// Interned strings, length-prefixed in pages.
#[derive(Default)]
struct Interner {
    pages: Vec<Vec<u8>>,
    /// Per string: `page << 32 | offset`.
    starts: Vec<u64>,
    /// Hash → newest string with it; older ones chain through `next`.
    index: FxHashMap<u64, u32>,
    next: Vec<u32>,
}

const NONE: u32 = u32::MAX;
const STRING_PAGE: usize = 4 << 20;

fn hash_of(v: impl Hash) -> u64 {
    let mut h = FxHasher::default();
    v.hash(&mut h);
    h.finish()
}

impl Interner {
    fn get(&self, i: u32) -> &str {
        let s = self.starts[i as usize];
        let page = &self.pages[(s >> 32) as usize];
        let mut pos = s as u32 as usize;
        let (mut len, mut shift) = (0usize, 0);
        loop {
            let b = page[pos];
            pos += 1;
            len |= ((b & 0x7f) as usize) << shift;
            if b < 0x80 {
                break;
            }
            shift += 7;
        }
        // SAFETY: only whole `&str`s were written, so this is one.
        unsafe { std::str::from_utf8_unchecked(&page[pos..pos + len]) }
    }

    fn intern(&mut self, s: &str) -> u32 {
        let h = hash_of(s);
        let head = self.index.get(&h).copied().unwrap_or(NONE);
        let mut i = head;
        while i != NONE {
            if self.get(i) == s {
                return i;
            }
            i = self.next[i as usize];
        }
        let need = s.len() + 10;
        if self.pages.last().is_none_or(|p| p.len() + need > p.capacity()) {
            if let Some(p) = self.pages.last_mut() {
                p.shrink_to_fit();
            }
            self.pages.push(Vec::with_capacity(STRING_PAGE.max(need)));
        }
        let page = self.pages.len() - 1;
        let p = &mut self.pages[page];
        self.starts.push((page as u64) << 32 | p.len() as u64);
        let mut len = s.len();
        while len >= 0x80 {
            p.push((len as u8) | 0x80);
            len >>= 7;
        }
        p.push(len as u8);
        p.extend_from_slice(s.as_bytes());
        let id = self.starts.len() as u32 - 1;
        self.next.push(head);
        self.index.insert(h, id);
        id
    }

    fn seal(&mut self) {
        self.index = FxHashMap::default();
        self.next = Vec::new();
        self.starts.shrink_to_fit();
        if let Some(p) = self.pages.last_mut() {
            p.shrink_to_fit();
        }
    }
}

/// An attribute with its key and string value interned (16 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PackedAttribute {
    key: u32,
    min_zoom: u8,
    /// 0 string, 1 float, 2 int, 3 bool.
    kind: u8,
    value: [u32; 2],
}

/// An attribute value as stored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AttributeRef<'a> {
    /// Text.
    String(&'a str),
    /// A float (`AttributeNumeric`).
    Float(f64),
    /// An integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
}

/// Distinct attribute sets, back to back, with keys and string values
/// interned: an attribute takes 16 bytes and each distinct string is kept
/// once.
#[derive(Default)]
pub struct AttributeStore {
    attrs: Vec<PackedAttribute>,
    /// Per set: the exclusive end of its attributes.
    ends: Vec<u32>,
    keys: Interner,
    strings: Interner,
    /// Hash → newest set with it; older ones chain through `next`.
    index: FxHashMap<u64, u32>,
    next: Vec<u32>,
}

impl AttributeStore {
    /// Number of sets.
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    fn set(&self, i: u32) -> &[PackedAttribute] {
        let start = if i == 0 { 0 } else { self.ends[i as usize - 1] as usize };
        &self.attrs[start..self.ends[i as usize] as usize]
    }

    /// Store a set (sorted by key), or find it stored; its index.
    pub fn insert(&mut self, mut attrs: Vec<Attribute>) -> u32 {
        attrs.sort_by(|a, b| a.key.cmp(&b.key));
        let packed: Vec<PackedAttribute> = attrs
            .iter()
            .map(|a| {
                let (kind, value) = match &a.value {
                    AttributeValue::String(s) => (0, [self.strings.intern(s), 0]),
                    AttributeValue::Float(f) => (1, split(f.to_bits())),
                    AttributeValue::Int(i) => (2, split(*i as u64)),
                    AttributeValue::Bool(b) => (3, [*b as u32, 0]),
                };
                PackedAttribute {
                    key: self.keys.intern(&a.key),
                    min_zoom: a.min_zoom,
                    kind,
                    value,
                }
            })
            .collect();
        let h = hash_of(&packed);
        let head = self.index.get(&h).copied().unwrap_or(NONE);
        let mut i = head;
        while i != NONE {
            if self.set(i) == packed.as_slice() {
                return i;
            }
            i = self.next[i as usize];
        }
        self.attrs.extend_from_slice(&packed);
        self.ends.push(self.attrs.len() as u32);
        let id = self.ends.len() as u32 - 1;
        self.next.push(head);
        self.index.insert(h, id);
        id
    }

    /// Set `i`'s attributes: key, value and minimum zoom.
    pub fn get(&self, i: u32) -> impl Iterator<Item = (&str, AttributeRef<'_>, u8)> + '_ {
        self.set(i).iter().map(move |a| {
            let v = match a.kind {
                0 => AttributeRef::String(self.strings.get(a.value[0])),
                1 => AttributeRef::Float(f64::from_bits(join(a.value))),
                2 => AttributeRef::Int(join(a.value) as i64),
                _ => AttributeRef::Bool(a.value[0] != 0),
            };
            (self.keys.get(a.key), v, a.min_zoom)
        })
    }

    fn seal(&mut self) {
        self.index = FxHashMap::default();
        self.next = Vec::new();
        self.attrs.shrink_to_fit();
        self.ends.shrink_to_fit();
        self.keys.seal();
        self.strings.seal();
    }
}

fn split(v: u64) -> [u32; 2] {
    [v as u32, (v >> 32) as u32]
}

fn join(v: [u32; 2]) -> u64 {
    v[0] as u64 | (v[1] as u64) << 32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(key: &str, value: AttributeValue, min_zoom: u8) -> Attribute {
        Attribute {
            key: key.into(),
            value,
            min_zoom,
        }
    }

    #[test]
    fn attribute_sets_dedupe_and_round_trip() {
        let mut s = AttributeStore::default();
        let a = vec![
            attr("name", AttributeValue::String("Amsterdam".into()), 0),
            attr("class", AttributeValue::String("city".into()), 0),
            attr("rank", AttributeValue::Int(-3), 0),
            attr("pop", AttributeValue::Float(1.5e6), 10),
            attr("capital", AttributeValue::Bool(true), 0),
        ];
        let i = s.insert(a.clone());
        let mut b = a.clone();
        b.reverse();
        assert_eq!(s.insert(b), i);
        let j = s.insert(vec![attr("name", AttributeValue::String("city".into()), 0)]);
        assert_ne!(i, j);
        assert_eq!(s.len(), 2);
        let got: Vec<_> = s.get(i).collect();
        assert_eq!(
            got,
            vec![
                ("capital", AttributeRef::Bool(true), 0),
                ("class", AttributeRef::String("city"), 0),
                ("name", AttributeRef::String("Amsterdam"), 0),
                ("pop", AttributeRef::Float(1.5e6), 10),
                ("rank", AttributeRef::Int(-3), 0),
            ]
        );
        s.seal();
        assert_eq!(s.get(j).next(), Some(("name", AttributeRef::String("city"), 0)));
        assert_eq!(std::mem::size_of::<PackedAttribute>(), 16);
        assert_eq!(std::mem::size_of::<OutputObject>(), 16);
    }

    #[test]
    fn interning_survives_hash_collisions_and_pages() {
        let mut i = Interner::default();
        let words: Vec<String> = (0..200_000).map(|n| format!("w{n}")).collect();
        let ids: Vec<u32> = words.iter().map(|w| i.intern(w)).collect();
        let long = "x".repeat(STRING_PAGE + 5);
        let l = i.intern(&long);
        for (w, &id) in words.iter().zip(&ids) {
            assert_eq!(i.intern(w), id);
            assert_eq!(i.get(id), w);
        }
        assert_eq!(i.get(l), long);
        assert!(i.pages.len() >= 2);
    }
}
