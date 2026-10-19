//! Writing PBF files: zlib-compressed blocks of dense nodes, ways and
//! relations, sorted however the caller supplies them.

use std::collections::HashMap;
use std::io::Write;

use flate2::write::ZlibEncoder;

use crate::block::{ElementType, Info};
use crate::header::HeaderBbox;
use crate::Error;

/// Elements per block (as osmium writes).
pub const ELEMENTS_PER_BLOCK: usize = 8000;

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn key(out: &mut Vec<u8>, field: u32, wire: u8) {
    varint(out, ((field as u64) << 3) | wire as u64);
}

fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn bytes_field(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    key(out, field, 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn varint_field(out: &mut Vec<u8>, field: u32, v: u64) {
    key(out, field, 0);
    varint(out, v);
}

fn packed(out: &mut Vec<u8>, field: u32, vals: impl IntoIterator<Item = u64>) {
    let mut buf = Vec::new();
    for v in vals {
        varint(&mut buf, v);
    }
    if !buf.is_empty() {
        bytes_field(out, field, &buf);
    }
}

fn delta(vals: impl IntoIterator<Item = i64>) -> impl Iterator<Item = u64> {
    let mut prev = 0i64;
    vals.into_iter().map(move |v| {
        let d = v.wrapping_sub(prev);
        prev = v;
        zz(d)
    })
}

#[derive(Default)]
struct Strings {
    table: Vec<String>,
    index: HashMap<String, u32>,
}

impl Strings {
    fn new() -> Self {
        let mut s = Self::default();
        s.get("");
        s
    }

    fn get(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.table.len() as u32;
        self.table.push(s.to_string());
        self.index.insert(s.to_string(), i);
        i
    }
}

struct PendingNode {
    id: i64,
    lat: i32,
    lon: i32,
    tags: Vec<(u32, u32)>,
    info: Option<(Info, u32)>,
}

struct PendingWay {
    id: i64,
    refs: Vec<i64>,
    locs: Vec<(i32, i32)>,
    tags: Vec<(u32, u32)>,
    info: Option<(Info, u32)>,
}

struct PendingRelation {
    id: i64,
    members: Vec<(i64, ElementType, u32)>,
    tags: Vec<(u32, u32)>,
    info: Option<(Info, u32)>,
}

/// Header options for [`PbfWriter`].
#[derive(Clone, Debug, Default)]
pub struct WriterOptions {
    /// Bounding box to record.
    pub bbox: Option<HeaderBbox>,
    /// Declare `Sort.Type_then_ID` (the caller must write in that order).
    pub sorted: bool,
    /// Declare `LocationsOnWays` (ways must carry locations).
    pub locations_on_ways: bool,
    /// Writing program.
    pub writing_program: Option<String>,
    /// Compress blocks with zlib (default on in [`PbfWriter::new`]).
    pub uncompressed: bool,
}

/// Writes a PBF file.
pub struct PbfWriter<W: Write> {
    out: W,
    opts: WriterOptions,
    strings: Strings,
    nodes: Vec<PendingNode>,
    ways: Vec<PendingWay>,
    relations: Vec<PendingRelation>,
    header_written: bool,
}

impl<W: Write> PbfWriter<W> {
    /// A writer with default options.
    pub fn new(out: W) -> Self {
        Self::with_options(out, WriterOptions::default())
    }

    /// A writer with options.
    pub fn with_options(out: W, opts: WriterOptions) -> Self {
        Self {
            out,
            opts,
            strings: Strings::new(),
            nodes: Vec::new(),
            ways: Vec::new(),
            relations: Vec::new(),
            header_written: false,
        }
    }

    fn tags(&mut self, tags: &[(&str, &str)]) -> Vec<(u32, u32)> {
        tags.iter()
            .map(|(k, v)| (self.strings.get(k), self.strings.get(v)))
            .collect()
    }

    fn info(&mut self, info: Option<(&Info, &str)>) -> Option<(Info, u32)> {
        info.map(|(i, user)| (*i, self.strings.get(user)))
    }

    /// Add a node (coordinates at 1e-7 degrees).
    pub fn node(&mut self, id: i64, lat_e7: i32, lon_e7: i32, tags: &[(&str, &str)]) -> Result<(), Error> {
        self.node_with_info(id, lat_e7, lon_e7, tags, None)
    }

    /// Add a node with metadata.
    pub fn node_with_info(
        &mut self,
        id: i64,
        lat_e7: i32,
        lon_e7: i32,
        tags: &[(&str, &str)],
        info: Option<(&Info, &str)>,
    ) -> Result<(), Error> {
        self.flush_if(!self.ways.is_empty() || !self.relations.is_empty())?;
        let tags = self.tags(tags);
        let info = self.info(info);
        self.nodes.push(PendingNode {
            id,
            lat: lat_e7,
            lon: lon_e7,
            tags,
            info,
        });
        self.flush_if(self.nodes.len() >= ELEMENTS_PER_BLOCK)
    }

    /// Add a way.
    pub fn way(&mut self, id: i64, refs: &[i64], tags: &[(&str, &str)]) -> Result<(), Error> {
        self.way_full(id, refs, &[], tags, None)
    }

    /// Add a way with node locations (`LocationsOnWays`) and metadata.
    pub fn way_full(
        &mut self,
        id: i64,
        refs: &[i64],
        locations: &[(i32, i32)],
        tags: &[(&str, &str)],
        info: Option<(&Info, &str)>,
    ) -> Result<(), Error> {
        self.flush_if(!self.nodes.is_empty() || !self.relations.is_empty())?;
        let tags = self.tags(tags);
        let info = self.info(info);
        self.ways.push(PendingWay {
            id,
            refs: refs.to_vec(),
            locs: locations.to_vec(),
            tags,
            info,
        });
        self.flush_if(self.ways.len() >= ELEMENTS_PER_BLOCK)
    }

    /// Add a relation; members are `(id, type, role)`.
    pub fn relation(
        &mut self,
        id: i64,
        members: &[(i64, ElementType, &str)],
        tags: &[(&str, &str)],
    ) -> Result<(), Error> {
        self.relation_with_info(id, members, tags, None)
    }

    /// Add a relation with metadata.
    pub fn relation_with_info(
        &mut self,
        id: i64,
        members: &[(i64, ElementType, &str)],
        tags: &[(&str, &str)],
        info: Option<(&Info, &str)>,
    ) -> Result<(), Error> {
        self.flush_if(!self.nodes.is_empty() || !self.ways.is_empty())?;
        let tags = self.tags(tags);
        let info = self.info(info);
        let members = members
            .iter()
            .map(|&(id, t, role)| (id, t, self.strings.get(role)))
            .collect();
        self.relations.push(PendingRelation {
            id,
            members,
            tags,
            info,
        });
        self.flush_if(self.relations.len() >= ELEMENTS_PER_BLOCK)
    }

    fn flush_if(&mut self, cond: bool) -> Result<(), Error> {
        if cond {
            self.flush()?;
        }
        Ok(())
    }

    fn write_header(&mut self) -> Result<(), Error> {
        if self.header_written {
            return Ok(());
        }
        self.header_written = true;
        let mut h = Vec::new();
        if let Some(b) = self.opts.bbox {
            let mut bb = Vec::new();
            for (f, v) in [(1, b.min_lon), (2, b.max_lon), (3, b.max_lat), (4, b.min_lat)] {
                varint_field(&mut bb, f, zz((v * 1e9).round() as i64));
            }
            bytes_field(&mut h, 1, &bb);
        }
        bytes_field(&mut h, 4, b"OsmSchema-V0.6");
        bytes_field(&mut h, 4, b"DenseNodes");
        if self.opts.sorted {
            bytes_field(&mut h, 5, b"Sort.Type_then_ID");
        }
        if self.opts.locations_on_ways {
            bytes_field(&mut h, 5, b"LocationsOnWays");
        }
        let program = self
            .opts
            .writing_program
            .clone()
            .unwrap_or_else(|| "tilemaker-pbf".into());
        bytes_field(&mut h, 16, program.as_bytes());
        self.write_blob("OSMHeader", &h)
    }

    fn write_blob(&mut self, kind: &str, data: &[u8]) -> Result<(), Error> {
        let mut blob = Vec::new();
        if self.opts.uncompressed {
            bytes_field(&mut blob, 1, data);
        } else {
            varint_field(&mut blob, 2, data.len() as u64);
            let mut enc = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(data)?;
            bytes_field(&mut blob, 3, &enc.finish()?);
        }
        let mut header = Vec::new();
        bytes_field(&mut header, 1, kind.as_bytes());
        varint_field(&mut header, 3, blob.len() as u64);
        self.out.write_all(&(header.len() as u32).to_be_bytes())?;
        self.out.write_all(&header)?;
        self.out.write_all(&blob)?;
        Ok(())
    }

    fn info_message(info: &Info, user: u32) -> Vec<u8> {
        let mut m = Vec::new();
        varint_field(&mut m, 1, info.version as u64);
        varint_field(&mut m, 2, info.timestamp as u64);
        varint_field(&mut m, 3, info.changeset as u64);
        varint_field(&mut m, 4, info.uid as u64);
        varint_field(&mut m, 5, user as u64);
        if !info.visible {
            varint_field(&mut m, 6, 0);
        }
        m
    }

    fn keys_vals(out: &mut Vec<u8>, tags: &[(u32, u32)]) {
        packed(out, 2, tags.iter().map(|t| t.0 as u64));
        packed(out, 3, tags.iter().map(|t| t.1 as u64));
    }

    /// Write out the buffered elements as one block.
    pub fn flush(&mut self) -> Result<(), Error> {
        self.write_header()?;
        if self.nodes.is_empty() && self.ways.is_empty() && self.relations.is_empty() {
            return Ok(());
        }
        let mut group = Vec::new();
        if !self.nodes.is_empty() {
            let nodes = std::mem::take(&mut self.nodes);
            let mut dense = Vec::new();
            packed(&mut dense, 1, delta(nodes.iter().map(|n| n.id)));
            if nodes.iter().any(|n| n.info.is_some()) {
                let mut di = Vec::new();
                let get = |n: &PendingNode| n.info.unwrap_or((Info::default(), 0));
                packed(&mut di, 1, nodes.iter().map(|n| get(n).0.version as u64));
                packed(&mut di, 2, delta(nodes.iter().map(|n| get(n).0.timestamp)));
                packed(&mut di, 3, delta(nodes.iter().map(|n| get(n).0.changeset)));
                packed(&mut di, 4, delta(nodes.iter().map(|n| get(n).0.uid as i64)));
                packed(&mut di, 5, delta(nodes.iter().map(|n| get(n).1 as i64)));
                if nodes.iter().any(|n| !get(n).0.visible) {
                    packed(&mut di, 6, nodes.iter().map(|n| get(n).0.visible as u64));
                }
                bytes_field(&mut dense, 5, &di);
            }
            packed(&mut dense, 8, delta(nodes.iter().map(|n| n.lat as i64)));
            packed(&mut dense, 9, delta(nodes.iter().map(|n| n.lon as i64)));
            if nodes.iter().any(|n| !n.tags.is_empty()) {
                let mut kv = Vec::new();
                for n in &nodes {
                    for &(k, v) in &n.tags {
                        kv.push(k as u64);
                        kv.push(v as u64);
                    }
                    kv.push(0);
                }
                packed(&mut dense, 10, kv);
            }
            bytes_field(&mut group, 2, &dense);
        }
        for w in std::mem::take(&mut self.ways) {
            let mut m = Vec::new();
            varint_field(&mut m, 1, w.id as u64);
            Self::keys_vals(&mut m, &w.tags);
            if let Some((info, user)) = &w.info {
                bytes_field(&mut m, 4, &Self::info_message(info, *user));
            }
            packed(&mut m, 8, delta(w.refs.iter().copied()));
            packed(&mut m, 9, delta(w.locs.iter().map(|l| l.0 as i64)));
            packed(&mut m, 10, delta(w.locs.iter().map(|l| l.1 as i64)));
            bytes_field(&mut group, 3, &m);
        }
        for r in std::mem::take(&mut self.relations) {
            let mut m = Vec::new();
            varint_field(&mut m, 1, r.id as u64);
            Self::keys_vals(&mut m, &r.tags);
            if let Some((info, user)) = &r.info {
                bytes_field(&mut m, 4, &Self::info_message(info, *user));
            }
            packed(&mut m, 8, r.members.iter().map(|m| m.2 as u64));
            packed(&mut m, 9, delta(r.members.iter().map(|m| m.0)));
            packed(&mut m, 10, r.members.iter().map(|m| m.1 as u64));
            bytes_field(&mut group, 4, &m);
        }
        let mut block = Vec::new();
        let mut st = Vec::new();
        for s in &self.strings.table {
            bytes_field(&mut st, 1, s.as_bytes());
        }
        bytes_field(&mut block, 1, &st);
        bytes_field(&mut block, 2, &group);
        self.strings = Strings::new();
        self.write_blob("OSMData", &block)
    }

    /// Flush and return the underlying writer.
    pub fn finish(mut self) -> Result<W, Error> {
        self.flush()?;
        self.out.flush()?;
        Ok(self.out)
    }
}
