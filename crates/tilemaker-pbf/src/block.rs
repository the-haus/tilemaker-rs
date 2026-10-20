//! Decoded `PrimitiveBlock`s and borrowed views of their elements.

use crate::proto::{each_varint, zigzag, Reader, LEN};
use crate::Error;

/// What to decode from a block.
#[derive(Clone, Copy, Debug)]
pub struct DecodeOptions {
    /// Decode nodes (plain and dense).
    pub nodes: bool,
    /// Decode ways.
    pub ways: bool,
    /// Decode relations.
    pub relations: bool,
    /// Decode element metadata (version, timestamp, changeset, user).
    pub metadata: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self::ALL
    }
}

impl DecodeOptions {
    /// Everything except metadata.
    pub const ALL: Self = Self {
        nodes: true,
        ways: true,
        relations: true,
        metadata: false,
    };

    /// Everything including metadata.
    pub const WITH_METADATA: Self = Self {
        metadata: true,
        ..Self::ALL
    };
}

/// The type of a relation member (and of OSM elements generally).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ElementType {
    /// A node.
    Node = 0,
    /// A way.
    Way = 1,
    /// A relation.
    Relation = 2,
}

impl ElementType {
    /// `"node"`, `"way"` or `"relation"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ElementType::Node => "node",
            ElementType::Way => "way",
            ElementType::Relation => "relation",
        }
    }
}

const NO_INFO: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Default)]
struct Span {
    start: u32,
    end: u32,
}

impl Span {
    #[inline]
    fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

#[derive(Clone, Debug)]
struct NodeData {
    id: i64,
    lat: i32,
    lon: i32,
    tags: Span,
    info: u32,
}

#[derive(Clone, Debug)]
struct WayData {
    id: i64,
    tags: Span,
    refs: Span,
    locs: Span,
    info: u32,
}

#[derive(Clone, Debug)]
struct RelationData {
    id: i64,
    tags: Span,
    members: Span,
    info: u32,
}

#[derive(Clone, Copy, Debug)]
struct MemberData {
    id: i64,
    kind: ElementType,
    role: u32,
}

/// Element metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// Version number.
    pub version: i32,
    /// Seconds since the Unix epoch.
    pub timestamp: i64,
    /// Changeset id.
    pub changeset: i64,
    /// User id.
    pub uid: i32,
    /// Whether the element is visible (history files only).
    pub visible: bool,
}

/// What kinds of elements a block holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockKinds {
    /// Has nodes.
    pub nodes: bool,
    /// Has ways.
    pub ways: bool,
    /// Has relations.
    pub relations: bool,
}

/// A decoded block of OSM elements.
#[derive(Debug, Default)]
pub struct PrimitiveBlock {
    /// The string table: all strings back to back, and where each ends.
    str_data: String,
    str_ends: Vec<u32>,
    nodes: Vec<NodeData>,
    ways: Vec<WayData>,
    relations: Vec<RelationData>,
    tags: Vec<(u32, u32)>,
    refs: Vec<i64>,
    locs: Vec<(i32, i32)>,
    members: Vec<MemberData>,
    infos: Vec<(Info, u32)>,
    kinds: BlockKinds,
}

impl PrimitiveBlock {
    /// Decode a block from its decompressed bytes.
    pub fn decode(data: &[u8], opts: DecodeOptions) -> Result<Self, Error> {
        let mut block = PrimitiveBlock::default();
        let mut granularity: i64 = 100;
        let mut lat_offset: i64 = 0;
        let mut lon_offset: i64 = 0;
        let mut date_granularity: i64 = 1000;
        let mut groups: Vec<&[u8]> = Vec::new();

        let mut r = Reader::new(data);
        while let Some((field, wire)) = r.field()? {
            match (field, wire) {
                (1, LEN) => {
                    let table = r.bytes()?;
                    block.str_data.reserve(table.len());
                    let mut st = Reader::new(table);
                    while let Some((f, w)) = st.field()? {
                        if f == 1 && w == LEN {
                            let s = st.bytes()?;
                            match std::str::from_utf8(s) {
                                Ok(s) => block.str_data.push_str(s),
                                Err(_) => block.str_data.push_str(&String::from_utf8_lossy(s)),
                            }
                            block.str_ends.push(block.str_data.len() as u32);
                        } else {
                            st.skip(w)?;
                        }
                    }
                }
                (2, LEN) => groups.push(r.bytes()?),
                (17, _) => granularity = r.int64()?,
                (18, _) => date_granularity = r.int64()?,
                (19, _) => lat_offset = r.int64()?,
                (20, _) => lon_offset = r.int64()?,
                _ => r.skip(wire)?,
            }
        }
        if block.str_ends.is_empty() {
            block.str_ends.push(0);
        }

        let coords = Coords {
            granularity,
            lat_offset,
            lon_offset,
            date_granularity,
        };
        let mut scratch = Scratch::default();
        for g in groups {
            block.decode_group(g, &coords, opts, &mut scratch)?;
        }
        Ok(block)
    }

    fn decode_group(&mut self, data: &[u8], c: &Coords, opts: DecodeOptions, sc: &mut Scratch) -> Result<(), Error> {
        let mut r = Reader::new(data);
        while let Some((field, wire)) = r.field()? {
            match (field, wire) {
                (1, LEN) => {
                    self.kinds.nodes = true;
                    let b = r.bytes()?;
                    if opts.nodes {
                        self.decode_node(b, c, opts, sc)?;
                    }
                }
                (2, LEN) => {
                    self.kinds.nodes = true;
                    let b = r.bytes()?;
                    if opts.nodes {
                        self.decode_dense(b, c, opts)?;
                    }
                }
                (3, LEN) => {
                    self.kinds.ways = true;
                    let b = r.bytes()?;
                    if opts.ways {
                        self.decode_way(b, c, opts, sc)?;
                    }
                }
                (4, LEN) => {
                    self.kinds.relations = true;
                    let b = r.bytes()?;
                    if opts.relations {
                        self.decode_relation(b, c, opts, sc)?;
                    }
                }
                _ => r.skip(wire)?,
            }
        }
        Ok(())
    }

    fn string_index(&self, i: u64) -> Result<u32, Error> {
        if (i as usize) < self.str_ends.len() {
            Ok(i as u32)
        } else {
            Err(Error::Corrupt("string table index out of range"))
        }
    }

    fn push_tags(&mut self, keys: &[u32], vals: &[u32]) -> Result<Span, Error> {
        if keys.len() != vals.len() {
            return Err(Error::Corrupt("keys and values differ in length"));
        }
        let start = self.tags.len() as u32;
        for (&k, &v) in keys.iter().zip(vals) {
            let k = self.string_index(k as u64)?;
            let v = self.string_index(v as u64)?;
            self.tags.push((k, v));
        }
        Ok(Span {
            start,
            end: self.tags.len() as u32,
        })
    }

    fn decode_info(&mut self, data: &[u8], c: &Coords) -> Result<u32, Error> {
        let mut info = Info {
            visible: true,
            ..Info::default()
        };
        let mut user_sid = 0u32;
        let mut r = Reader::new(data);
        while let Some((field, wire)) = r.field()? {
            match field {
                1 => info.version = r.varint()? as i32,
                2 => info.timestamp = r.int64()?.wrapping_mul(c.date_granularity) / 1000,
                3 => info.changeset = r.int64()?,
                4 => info.uid = r.varint()? as i32,
                5 => user_sid = self.string_index(r.varint()?)?,
                6 => info.visible = r.varint()? != 0,
                _ => r.skip(wire)?,
            }
        }
        self.infos.push((info, user_sid));
        Ok(self.infos.len() as u32 - 1)
    }

    fn decode_node(&mut self, data: &[u8], c: &Coords, opts: DecodeOptions, sc: &mut Scratch) -> Result<(), Error> {
        let mut r = Reader::new(data);
        let (mut id, mut lat, mut lon) = (0i64, 0i64, 0i64);
        let Scratch { keys, vals, .. } = sc;
        keys.clear();
        vals.clear();
        let mut info = NO_INFO;
        while let Some((field, wire)) = r.field()? {
            match field {
                1 => id = r.sint64()?,
                2 => each_varint(&mut r, wire, |v| keys.push(v as u32))?,
                3 => each_varint(&mut r, wire, |v| vals.push(v as u32))?,
                4 if opts.metadata => {
                    let b = r.bytes()?;
                    info = self.decode_info(b, c)?;
                }
                8 => lat = r.sint64()?,
                9 => lon = r.sint64()?,
                _ => r.skip(wire)?,
            }
        }
        let tags = self.push_tags(keys, vals)?;
        self.nodes.push(NodeData {
            id,
            lat: c.lat(lat),
            lon: c.lon(lon),
            tags,
            info,
        });
        Ok(())
    }

    fn decode_dense(&mut self, data: &[u8], c: &Coords, opts: DecodeOptions) -> Result<(), Error> {
        let mut r = Reader::new(data);
        let (mut ids, mut lats, mut lons, mut kv): (&[u8], &[u8], &[u8], &[u8]) = (&[], &[], &[], &[]);
        let mut dense_info: &[u8] = &[];
        while let Some((field, wire)) = r.field()? {
            match (field, wire) {
                (1, LEN) => ids = r.bytes()?,
                (5, LEN) => dense_info = r.bytes()?,
                (8, LEN) => lats = r.bytes()?,
                (9, LEN) => lons = r.bytes()?,
                (10, LEN) => kv = r.bytes()?,
                _ => r.skip(wire)?,
            }
        }

        let infos = if opts.metadata && !dense_info.is_empty() {
            Some(self.decode_dense_info(dense_info, c)?)
        } else {
            None
        };

        let n = count_varints(ids);
        self.nodes.reserve(n);
        if !kv.is_empty() {
            // Pairs plus one 0 per node.
            self.tags.reserve(count_varints(kv).saturating_sub(n) / 2);
        }
        let mut id_r = Reader::new(ids);
        let mut lat_r = Reader::new(lats);
        let mut lon_r = Reader::new(lons);
        let mut kv_r = Reader::new(kv);
        let (mut id, mut lat, mut lon) = (0i64, 0i64, 0i64);
        let mut n = 0usize;
        while !id_r.is_empty() {
            id = id.wrapping_add(id_r.sint64()?);
            if lat_r.is_empty() || lon_r.is_empty() {
                return Err(Error::Corrupt("dense nodes: fewer coordinates than ids"));
            }
            lat = lat.wrapping_add(lat_r.sint64()?);
            lon = lon.wrapping_add(lon_r.sint64()?);
            let start = self.tags.len() as u32;
            if !kv.is_empty() {
                loop {
                    if kv_r.is_empty() {
                        break;
                    }
                    let k = kv_r.varint()?;
                    if k == 0 {
                        break;
                    }
                    let v = kv_r.varint()?;
                    let k = self.string_index(k)?;
                    let v = self.string_index(v)?;
                    self.tags.push((k, v));
                }
            }
            let info = match &infos {
                Some(list) => list.get(n).copied().unwrap_or(NO_INFO),
                None => NO_INFO,
            };
            self.nodes.push(NodeData {
                id,
                lat: c.lat(lat),
                lon: c.lon(lon),
                tags: Span {
                    start,
                    end: self.tags.len() as u32,
                },
                info,
            });
            n += 1;
        }
        Ok(())
    }

    fn decode_dense_info(&mut self, data: &[u8], c: &Coords) -> Result<Vec<u32>, Error> {
        let mut r = Reader::new(data);
        let mut version = Vec::new();
        let mut timestamp = Vec::new();
        let mut changeset = Vec::new();
        let mut uid = Vec::new();
        let mut user = Vec::new();
        let mut visible = Vec::new();
        while let Some((field, wire)) = r.field()? {
            match field {
                1 => each_varint(&mut r, wire, |v| version.push(v as i32))?,
                2 => each_varint(&mut r, wire, |v| timestamp.push(zigzag(v)))?,
                3 => each_varint(&mut r, wire, |v| changeset.push(zigzag(v)))?,
                4 => each_varint(&mut r, wire, |v| uid.push(zigzag(v)))?,
                5 => each_varint(&mut r, wire, |v| user.push(zigzag(v)))?,
                6 => each_varint(&mut r, wire, |v| visible.push(v != 0))?,
                _ => r.skip(wire)?,
            }
        }
        let n = version.len().max(timestamp.len());
        let (mut ts, mut cs, mut u, mut us) = (0i64, 0i64, 0i64, 0i64);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            ts = ts.wrapping_add(timestamp.get(i).copied().unwrap_or(0));
            cs = cs.wrapping_add(changeset.get(i).copied().unwrap_or(0));
            u = u.wrapping_add(uid.get(i).copied().unwrap_or(0));
            us = us.wrapping_add(user.get(i).copied().unwrap_or(0));
            let user_sid = if us >= 0 && (us as usize) < self.str_ends.len() {
                us as u32
            } else {
                0
            };
            self.infos.push((
                Info {
                    version: version.get(i).copied().unwrap_or(0),
                    timestamp: ts.wrapping_mul(c.date_granularity) / 1000,
                    changeset: cs,
                    uid: u as i32,
                    visible: visible.get(i).copied().unwrap_or(true),
                },
                user_sid,
            ));
            out.push(self.infos.len() as u32 - 1);
        }
        Ok(out)
    }

    fn decode_way(&mut self, data: &[u8], c: &Coords, opts: DecodeOptions, sc: &mut Scratch) -> Result<(), Error> {
        let mut r = Reader::new(data);
        let mut id = 0i64;
        let Scratch {
            keys, vals, lats, lons, ..
        } = sc;
        keys.clear();
        vals.clear();
        lats.clear();
        lons.clear();
        let mut info = NO_INFO;
        let refs_start = self.refs.len() as u32;
        let locs_start = self.locs.len() as u32;
        while let Some((field, wire)) = r.field()? {
            match field {
                1 => id = r.int64()?,
                2 => each_varint(&mut r, wire, |v| keys.push(v as u32))?,
                3 => each_varint(&mut r, wire, |v| vals.push(v as u32))?,
                4 if opts.metadata => {
                    let b = r.bytes()?;
                    info = self.decode_info(b, c)?;
                }
                8 => {
                    let mut acc = 0i64;
                    let refs = &mut self.refs;
                    if wire == LEN {
                        refs.reserve(count_varints(r.peek_bytes()?));
                    }
                    each_varint(&mut r, wire, |v| {
                        acc = acc.wrapping_add(zigzag(v));
                        refs.push(acc);
                    })?;
                }
                9 => {
                    let mut acc = 0i64;
                    each_varint(&mut r, wire, |v| {
                        acc = acc.wrapping_add(zigzag(v));
                        lats.push(acc);
                    })?;
                }
                10 => {
                    let mut acc = 0i64;
                    each_varint(&mut r, wire, |v| {
                        acc = acc.wrapping_add(zigzag(v));
                        lons.push(acc);
                    })?;
                }
                _ => r.skip(wire)?,
            }
        }
        if !lats.is_empty() && lats.len() == lons.len() {
            for (&la, &lo) in lats.iter().zip(lons.iter()) {
                self.locs.push((c.lat(la), c.lon(lo)));
            }
        }
        let tags = self.push_tags(keys, vals)?;
        self.ways.push(WayData {
            id,
            tags,
            refs: Span {
                start: refs_start,
                end: self.refs.len() as u32,
            },
            locs: Span {
                start: locs_start,
                end: self.locs.len() as u32,
            },
            info,
        });
        Ok(())
    }

    fn decode_relation(&mut self, data: &[u8], c: &Coords, opts: DecodeOptions, sc: &mut Scratch) -> Result<(), Error> {
        let mut r = Reader::new(data);
        let mut id = 0i64;
        let Scratch {
            keys,
            vals,
            roles,
            memids,
            types,
            ..
        } = sc;
        keys.clear();
        vals.clear();
        roles.clear();
        memids.clear();
        types.clear();
        let mut info = NO_INFO;
        while let Some((field, wire)) = r.field()? {
            match field {
                1 => id = r.int64()?,
                2 => each_varint(&mut r, wire, |v| keys.push(v as u32))?,
                3 => each_varint(&mut r, wire, |v| vals.push(v as u32))?,
                4 if opts.metadata => {
                    let b = r.bytes()?;
                    info = self.decode_info(b, c)?;
                }
                8 => each_varint(&mut r, wire, |v| roles.push(v))?,
                9 => {
                    let mut acc = 0i64;
                    each_varint(&mut r, wire, |v| {
                        acc = acc.wrapping_add(zigzag(v));
                        memids.push(acc);
                    })?;
                }
                10 => each_varint(&mut r, wire, |v| types.push(v))?,
                _ => r.skip(wire)?,
            }
        }
        if memids.len() != types.len() || memids.len() != roles.len() {
            return Err(Error::Corrupt("relation members differ in length"));
        }
        let start = self.members.len() as u32;
        for i in 0..memids.len() {
            let kind = match types[i] {
                0 => ElementType::Node,
                1 => ElementType::Way,
                2 => ElementType::Relation,
                _ => return Err(Error::Corrupt("unknown relation member type")),
            };
            let role = self.string_index(roles[i])?;
            self.members.push(MemberData {
                id: memids[i],
                kind,
                role,
            });
        }
        let tags = self.push_tags(keys, vals)?;
        self.relations.push(RelationData {
            id,
            tags,
            members: Span {
                start,
                end: self.members.len() as u32,
            },
            info,
        });
        Ok(())
    }

    /// Number of strings in the block's string table.
    pub fn string_count(&self) -> usize {
        self.str_ends.len()
    }

    /// The block's string table.
    pub fn strings(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        (0..self.str_ends.len() as u32).map(move |i| self.s(i))
    }

    /// String `i` of the table.
    pub fn string(&self, i: u32) -> Option<&str> {
        ((i as usize) < self.str_ends.len()).then(|| self.s(i))
    }

    /// Kinds of elements present (even those not decoded).
    pub fn kinds(&self) -> BlockKinds {
        self.kinds
    }

    /// Number of decoded nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of decoded ways.
    pub fn way_count(&self) -> usize {
        self.ways.len()
    }

    /// Number of decoded relations.
    pub fn relation_count(&self) -> usize {
        self.relations.len()
    }

    /// Iterate the nodes.
    pub fn nodes(&self) -> impl ExactSizeIterator<Item = Node<'_>> + '_ {
        self.nodes.iter().map(move |d| Node { block: self, d })
    }

    /// Iterate the ways.
    pub fn ways(&self) -> impl ExactSizeIterator<Item = Way<'_>> + '_ {
        self.ways.iter().map(move |d| Way { block: self, d })
    }

    /// Iterate the relations.
    pub fn relations(&self) -> impl ExactSizeIterator<Item = Relation<'_>> + '_ {
        self.relations.iter().map(move |d| Relation { block: self, d })
    }

    /// The `i`th relation.
    pub fn relation(&self, i: usize) -> Relation<'_> {
        Relation {
            block: self,
            d: &self.relations[i],
        }
    }

    /// Index of a string in the table, if present.
    pub fn find_string(&self, s: &str) -> Option<u32> {
        self.strings().position(|x| x == s).map(|i| i as u32)
    }

    #[inline]
    fn s(&self, i: u32) -> &str {
        let end = self.str_ends[i as usize] as usize;
        let start = match i {
            0 => 0,
            _ => self.str_ends[i as usize - 1] as usize,
        };
        // Ends are char boundaries: each string was pushed whole.
        &self.str_data[start..end]
    }

    fn tags_of(&self, span: Span) -> Tags<'_> {
        Tags {
            block: self,
            pairs: &self.tags[span.range()],
        }
    }

    fn info_of(&self, i: u32) -> Option<InfoRef<'_>> {
        (i != NO_INFO).then(|| InfoRef {
            block: self,
            info: &self.infos[i as usize].0,
            user_sid: self.infos[i as usize].1,
        })
    }
}

/// Buffers reused across a block's elements.
#[derive(Default)]
struct Scratch {
    keys: Vec<u32>,
    vals: Vec<u32>,
    lats: Vec<i64>,
    lons: Vec<i64>,
    roles: Vec<u64>,
    memids: Vec<i64>,
    types: Vec<u64>,
}

/// Number of varints in a packed field (each ends with a byte below 0x80).
#[inline]
fn count_varints(b: &[u8]) -> usize {
    b.iter().filter(|&&x| x < 0x80).count()
}

struct Coords {
    granularity: i64,
    lat_offset: i64,
    lon_offset: i64,
    date_granularity: i64,
}

impl Coords {
    /// To 1e-7 degrees (OSM's usual fixed point).
    #[inline]
    fn lat(&self, raw: i64) -> i32 {
        (self.lat_offset.wrapping_add(self.granularity.wrapping_mul(raw)) / 100) as i32
    }

    #[inline]
    fn lon(&self, raw: i64) -> i32 {
        (self.lon_offset.wrapping_add(self.granularity.wrapping_mul(raw)) / 100) as i32
    }
}

/// An element's tags.
#[derive(Clone, Copy)]
pub struct Tags<'a> {
    block: &'a PrimitiveBlock,
    pairs: &'a [(u32, u32)],
}

impl<'a> Tags<'a> {
    /// Number of tags.
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Whether there are no tags.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// The value of `key`, if present.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        let block = self.block;
        self.pairs
            .iter()
            .find(|(k, _)| block.s(*k) == key)
            .map(|(_, v)| block.s(*v))
    }

    /// Whether `key` is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Iterate key/value pairs.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&'a str, &'a str)> + 'a {
        let block = self.block;
        self.pairs.iter().map(move |&(k, v)| (block.s(k), block.s(v)))
    }

    /// The raw string-table indices of the pairs.
    pub fn raw(&self) -> &'a [(u32, u32)] {
        self.pairs
    }
}

impl std::fmt::Debug for Tags<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// Element metadata with its user name resolved.
#[derive(Clone, Copy)]
pub struct InfoRef<'a> {
    block: &'a PrimitiveBlock,
    info: &'a Info,
    user_sid: u32,
}

impl<'a> InfoRef<'a> {
    /// The raw metadata.
    pub fn info(&self) -> &'a Info {
        self.info
    }

    /// User name.
    pub fn user(&self) -> &'a str {
        self.block.s(self.user_sid)
    }
}

impl std::ops::Deref for InfoRef<'_> {
    type Target = Info;

    fn deref(&self) -> &Info {
        self.info
    }
}

/// A node in a block.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    block: &'a PrimitiveBlock,
    d: &'a NodeData,
}

impl<'a> Node<'a> {
    /// Id.
    pub fn id(&self) -> i64 {
        self.d.id
    }
    /// Latitude × 1e7.
    pub fn lat_e7(&self) -> i32 {
        self.d.lat
    }
    /// Longitude × 1e7.
    pub fn lon_e7(&self) -> i32 {
        self.d.lon
    }
    /// Latitude in degrees.
    pub fn lat(&self) -> f64 {
        self.d.lat as f64 / 1e7
    }
    /// Longitude in degrees.
    pub fn lon(&self) -> f64 {
        self.d.lon as f64 / 1e7
    }
    /// Tags.
    pub fn tags(&self) -> Tags<'a> {
        self.block.tags_of(self.d.tags)
    }
    /// Metadata, when decoded.
    pub fn info(&self) -> Option<InfoRef<'a>> {
        self.block.info_of(self.d.info)
    }
}

/// A way in a block.
#[derive(Clone, Copy)]
pub struct Way<'a> {
    block: &'a PrimitiveBlock,
    d: &'a WayData,
}

impl<'a> Way<'a> {
    /// Id.
    pub fn id(&self) -> i64 {
        self.d.id
    }
    /// Tags.
    pub fn tags(&self) -> Tags<'a> {
        self.block.tags_of(self.d.tags)
    }
    /// Node ids.
    pub fn refs(&self) -> &'a [i64] {
        &self.block.refs[self.d.refs.range()]
    }
    /// Node locations as (lat, lon) × 1e7, for files with the
    /// `LocationsOnWays` feature; empty otherwise.
    pub fn locations(&self) -> &'a [(i32, i32)] {
        &self.block.locs[self.d.locs.range()]
    }
    /// Metadata, when decoded.
    pub fn info(&self) -> Option<InfoRef<'a>> {
        self.block.info_of(self.d.info)
    }
}

/// A relation member.
#[derive(Clone, Copy, Debug)]
pub struct Member<'a> {
    /// Member id.
    pub id: i64,
    /// Member type.
    pub kind: ElementType,
    /// Member role.
    pub role: &'a str,
}

/// A relation in a block.
#[derive(Clone, Copy)]
pub struct Relation<'a> {
    block: &'a PrimitiveBlock,
    d: &'a RelationData,
}

impl<'a> Relation<'a> {
    /// Id.
    pub fn id(&self) -> i64 {
        self.d.id
    }
    /// Tags.
    pub fn tags(&self) -> Tags<'a> {
        self.block.tags_of(self.d.tags)
    }
    /// Number of members.
    pub fn member_count(&self) -> usize {
        self.d.members.range().len()
    }
    /// Members, in order.
    pub fn members(&self) -> impl ExactSizeIterator<Item = Member<'a>> + 'a {
        let block = self.block;
        block.members[self.d.members.range()].iter().map(move |m| Member {
            id: m.id,
            kind: m.kind,
            role: block.s(m.role),
        })
    }
    /// Metadata, when decoded.
    pub fn info(&self) -> Option<InfoRef<'a>> {
        self.block.info_of(self.d.info)
    }
}
