//! Writing PBF files: zlib-compressed blocks of dense nodes, ways and
//! relations, sorted however the caller supplies them.
//!
//! * [`PbfWriter`] writes element by element on the calling thread.
//! * [`BlockBuilder`] collects elements into one block; blocks built on
//!   many threads can be merged ([`BlockBuilder::append`]).
//! * [`ParallelWriter`] takes builders in order and encodes and compresses
//!   them on worker threads, writing in order (what extracts use).

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;

use crate::block::{ElementType, Info};
use crate::header::HeaderBbox;
use crate::Error;

/// Elements per block (as osmium writes).
pub const ELEMENTS_PER_BLOCK: usize = 8000;

/// A block's estimated encoded size above which no more is merged into it.
const BLOCK_BYTES: usize = 16 << 20;

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

/// The elements of one block, with its string table.
pub struct BlockBuilder {
    strings: Strings,
    nodes: Vec<PendingNode>,
    ways: Vec<PendingWay>,
    relations: Vec<PendingRelation>,
    /// Estimated encoded size.
    bytes: usize,
}

impl Default for BlockBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockBuilder {
    /// An empty block.
    pub fn new() -> Self {
        Self {
            strings: Strings::new(),
            nodes: Vec::new(),
            ways: Vec::new(),
            relations: Vec::new(),
            bytes: 0,
        }
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.nodes.len() + self.ways.len() + self.relations.len()
    }

    /// Whether it holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The type of its elements (the first present).
    pub fn kind(&self) -> Option<ElementType> {
        if !self.nodes.is_empty() {
            Some(ElementType::Node)
        } else if !self.ways.is_empty() {
            Some(ElementType::Way)
        } else if !self.relations.is_empty() {
            Some(ElementType::Relation)
        } else {
            None
        }
    }

    /// Estimated encoded size in bytes.
    pub fn estimated_bytes(&self) -> usize {
        self.bytes
    }

    fn tags<'a>(&mut self, tags: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<(u32, u32)> {
        tags.into_iter()
            .map(|(k, v)| {
                self.bytes += 4;
                (self.strings.get(k), self.strings.get(v))
            })
            .collect()
    }

    fn info(&mut self, info: Option<(&Info, &str)>) -> Option<(Info, u32)> {
        info.map(|(i, user)| {
            self.bytes += 12;
            (*i, self.strings.get(user))
        })
    }

    /// Add a node (coordinates at 1e-7 degrees).
    pub fn node<'a>(
        &mut self,
        id: i64,
        lat_e7: i32,
        lon_e7: i32,
        tags: impl IntoIterator<Item = (&'a str, &'a str)>,
        info: Option<(&Info, &str)>,
    ) {
        let tags = self.tags(tags);
        let info = self.info(info);
        self.bytes += 8;
        self.nodes.push(PendingNode {
            id,
            lat: lat_e7,
            lon: lon_e7,
            tags,
            info,
        });
    }

    /// Add a way (with node locations for `LocationsOnWays`, or none).
    pub fn way<'a>(
        &mut self,
        id: i64,
        refs: &[i64],
        locations: &[(i32, i32)],
        tags: impl IntoIterator<Item = (&'a str, &'a str)>,
        info: Option<(&Info, &str)>,
    ) {
        let tags = self.tags(tags);
        let info = self.info(info);
        self.bytes += 8 + refs.len() * 3 + locations.len() * 6;
        self.ways.push(PendingWay {
            id,
            refs: refs.to_vec(),
            locs: locations.to_vec(),
            tags,
            info,
        });
    }

    /// Add a relation; members are `(id, type, role)`.
    pub fn relation<'a>(
        &mut self,
        id: i64,
        members: impl IntoIterator<Item = (i64, ElementType, &'a str)>,
        tags: impl IntoIterator<Item = (&'a str, &'a str)>,
        info: Option<(&Info, &str)>,
    ) {
        let tags = self.tags(tags);
        let info = self.info(info);
        let members: Vec<(i64, ElementType, u32)> = members
            .into_iter()
            .map(|(id, t, role)| (id, t, self.strings.get(role)))
            .collect();
        self.bytes += 8 + members.len() * 6;
        self.relations.push(PendingRelation {
            id,
            members,
            tags,
            info,
        });
    }

    /// Move `other`'s elements to the end of this block.
    pub fn append(&mut self, other: BlockBuilder) {
        let map: Vec<u32> = other.strings.table.iter().map(|s| self.strings.get(s)).collect();
        let tags = |t: Vec<(u32, u32)>| -> Vec<(u32, u32)> {
            t.into_iter().map(|(k, v)| (map[k as usize], map[v as usize])).collect()
        };
        let info = |i: Option<(Info, u32)>| i.map(|(i, u)| (i, map[u as usize]));
        self.nodes.extend(other.nodes.into_iter().map(|n| PendingNode {
            tags: tags(n.tags),
            info: info(n.info),
            ..n
        }));
        self.ways.extend(other.ways.into_iter().map(|w| PendingWay {
            tags: tags(w.tags),
            info: info(w.info),
            ..w
        }));
        self.relations.extend(other.relations.into_iter().map(|r| {
            PendingRelation {
                tags: tags(r.tags),
                info: info(r.info),
                members: r
                    .members
                    .into_iter()
                    .map(|(id, t, role)| (id, t, map[role as usize]))
                    .collect(),
                ..r
            }
        }));
        self.bytes += other.bytes;
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

    /// The `PrimitiveBlock` bytes: one group per element type present.
    pub fn encode(&self) -> Vec<u8> {
        let mut block = Vec::with_capacity(self.bytes + 1024);
        let mut st = Vec::new();
        for s in &self.strings.table {
            bytes_field(&mut st, 1, s.as_bytes());
        }
        bytes_field(&mut block, 1, &st);
        let nodes = &self.nodes;
        if !nodes.is_empty() {
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
                for n in nodes {
                    for &(k, v) in &n.tags {
                        kv.push(k as u64);
                        kv.push(v as u64);
                    }
                    kv.push(0);
                }
                packed(&mut dense, 10, kv);
            }
            let mut group = Vec::new();
            bytes_field(&mut group, 2, &dense);
            bytes_field(&mut block, 2, &group);
        }
        if !self.ways.is_empty() {
            let mut group = Vec::new();
            for w in &self.ways {
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
            bytes_field(&mut block, 2, &group);
        }
        if !self.relations.is_empty() {
            let mut group = Vec::new();
            for r in &self.relations {
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
            bytes_field(&mut block, 2, &group);
        }
        block
    }
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

/// The `OSMHeader` block's bytes.
pub fn header_block(opts: &WriterOptions) -> Vec<u8> {
    let mut h = Vec::new();
    if let Some(b) = opts.bbox {
        let mut bb = Vec::new();
        for (f, v) in [(1, b.min_lon), (2, b.max_lon), (3, b.max_lat), (4, b.min_lat)] {
            varint_field(&mut bb, f, zz((v * 1e9).round() as i64));
        }
        bytes_field(&mut h, 1, &bb);
    }
    bytes_field(&mut h, 4, b"OsmSchema-V0.6");
    bytes_field(&mut h, 4, b"DenseNodes");
    if opts.sorted {
        bytes_field(&mut h, 5, b"Sort.Type_then_ID");
    }
    if opts.locations_on_ways {
        bytes_field(&mut h, 5, b"LocationsOnWays");
    }
    let program = opts.writing_program.clone().unwrap_or_else(|| "tilemaker-pbf".into());
    bytes_field(&mut h, 16, program.as_bytes());
    h
}

thread_local! {
    static COMPRESSOR: RefCell<libdeflater::Compressor> =
        RefCell::new(libdeflater::Compressor::new(libdeflater::CompressionLvl::default()));
}

/// A blob framed for the file: length, `BlobHeader`, `Blob` (zlib
/// unless `uncompressed`).
pub fn frame_blob(kind: &str, data: &[u8], uncompressed: bool) -> Result<Vec<u8>, Error> {
    let mut blob = Vec::new();
    if uncompressed {
        bytes_field(&mut blob, 1, data);
    } else {
        varint_field(&mut blob, 2, data.len() as u64);
        let z = COMPRESSOR.with(|c| {
            let mut c = c.borrow_mut();
            let mut out = vec![0u8; c.zlib_compress_bound(data.len())];
            let n = c
                .zlib_compress(data, &mut out)
                .map_err(|e| Error::Decompress(format!("compressing: {e:?}")))?;
            out.truncate(n);
            Ok::<_, Error>(out)
        })?;
        bytes_field(&mut blob, 3, &z);
    }
    let mut header = Vec::new();
    bytes_field(&mut header, 1, kind.as_bytes());
    varint_field(&mut header, 3, blob.len() as u64);
    let mut out = Vec::with_capacity(4 + header.len() + blob.len());
    out.extend_from_slice(&(header.len() as u32).to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&blob);
    Ok(out)
}

/// Writes a PBF file.
pub struct PbfWriter<W: Write> {
    out: W,
    opts: WriterOptions,
    block: BlockBuilder,
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
            block: BlockBuilder::new(),
            header_written: false,
        }
    }

    /// Flush when the block holds another type or is full.
    fn before(&mut self, kind: ElementType) -> Result<(), Error> {
        if self.block.kind().is_some_and(|k| k != kind) || self.block.len() >= ELEMENTS_PER_BLOCK {
            self.flush()?;
        }
        Ok(())
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
        self.before(ElementType::Node)?;
        self.block.node(id, lat_e7, lon_e7, tags.iter().copied(), info);
        Ok(())
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
        self.before(ElementType::Way)?;
        self.block.way(id, refs, locations, tags.iter().copied(), info);
        Ok(())
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
        self.before(ElementType::Relation)?;
        self.block
            .relation(id, members.iter().copied(), tags.iter().copied(), info);
        Ok(())
    }

    fn write_header(&mut self) -> Result<(), Error> {
        if !self.header_written {
            self.header_written = true;
            let h = header_block(&self.opts);
            self.out
                .write_all(&frame_blob("OSMHeader", &h, self.opts.uncompressed)?)?;
        }
        Ok(())
    }

    /// Write out the buffered elements as one block.
    pub fn flush(&mut self) -> Result<(), Error> {
        self.write_header()?;
        if self.block.is_empty() {
            return Ok(());
        }
        let block = std::mem::take(&mut self.block);
        self.out
            .write_all(&frame_blob("OSMData", &block.encode(), self.opts.uncompressed)?)?;
        Ok(())
    }

    /// Flush and return the underlying writer.
    pub fn finish(mut self) -> Result<W, Error> {
        self.flush()?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Counters from a [`ParallelWriter`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteStats {
    /// Nodes written.
    pub nodes: u64,
    /// Ways written.
    pub ways: u64,
    /// Relations written.
    pub relations: u64,
    /// Data blocks written.
    pub blocks: u64,
    /// Bytes written.
    pub bytes: u64,
}

/// Writes blocks in the order given, encoding and compressing them on
/// worker threads. Small blocks are merged up to [`ELEMENTS_PER_BLOCK`]
/// elements (keeping one element type per block).
pub struct ParallelWriter<W: Write + Send + 'static> {
    current: BlockBuilder,
    tx: Option<crossbeam_channel::Sender<BlockBuilder>>,
    thread: Option<std::thread::JoinHandle<Result<(W, u64), Error>>>,
    stats: WriteStats,
}

impl<W: Write + Send + 'static> ParallelWriter<W> {
    /// Write the header now; blocks follow as they are pushed.
    pub fn new(mut out: W, opts: WriterOptions, threads: usize) -> Result<Self, Error> {
        let threads = threads.max(1);
        let header = frame_blob("OSMHeader", &header_block(&opts), opts.uncompressed)?;
        out.write_all(&header)?;
        let (tx, rx) = crossbeam_channel::bounded::<BlockBuilder>(threads * 2);
        let uncompressed = opts.uncompressed;
        let thread = std::thread::Builder::new()
            .name("pbf-writer".into())
            .spawn(move || {
                let blobs = crate::parallel::ordered_map(rx, threads, threads * 2, move |b: BlockBuilder| {
                    frame_blob("OSMData", &b.encode(), uncompressed)
                });
                let mut bytes = header.len() as u64;
                for blob in blobs {
                    let blob = blob?;
                    out.write_all(&blob)?;
                    bytes += blob.len() as u64;
                }
                out.flush()?;
                Ok((out, bytes))
            })
            .map_err(Error::Io)?;
        Ok(Self {
            current: BlockBuilder::new(),
            tx: Some(tx),
            thread: Some(thread),
            stats: WriteStats::default(),
        })
    }

    fn send(&mut self, b: BlockBuilder) -> Result<(), Error> {
        if b.is_empty() {
            return Ok(());
        }
        self.stats.nodes += b.nodes.len() as u64;
        self.stats.ways += b.ways.len() as u64;
        self.stats.relations += b.relations.len() as u64;
        self.stats.blocks += 1;
        let sent = self.tx.as_ref().is_some_and(|tx| tx.send(b).is_ok());
        if !sent {
            // The writer thread stopped: its error comes from `finish`.
            return Err(self.join().err().unwrap_or(Error::Corrupt("writer stopped")));
        }
        Ok(())
    }

    /// Add a block's elements (after everything pushed before).
    pub fn push(&mut self, b: BlockBuilder) -> Result<(), Error> {
        let Some(kind) = b.kind() else { return Ok(()) };
        let types = [!b.nodes.is_empty(), !b.ways.is_empty(), !b.relations.is_empty()];
        let mixed = types.iter().filter(|&&t| t).count() > 1;
        if self.current.kind().is_some_and(|k| k != kind)
            || mixed
            || self.current.len() + b.len() > ELEMENTS_PER_BLOCK
            || self.current.estimated_bytes() + b.estimated_bytes() > BLOCK_BYTES
        {
            let full = std::mem::take(&mut self.current);
            self.send(full)?;
        }
        if mixed {
            // Never mix types in one block: send each part on its own.
            return self.send(b);
        }
        if self.current.is_empty() && b.len() >= ELEMENTS_PER_BLOCK {
            return self.send(b);
        }
        self.current.append(b);
        if self.current.len() >= ELEMENTS_PER_BLOCK || self.current.estimated_bytes() > BLOCK_BYTES {
            let full = std::mem::take(&mut self.current);
            self.send(full)?;
        }
        Ok(())
    }

    fn join(&mut self) -> Result<(W, u64), Error> {
        self.tx.take();
        match self.thread.take() {
            Some(t) => t.join().map_err(|_| Error::Corrupt("writer thread panicked"))?,
            None => Err(Error::Corrupt("writer already finished")),
        }
    }

    /// Write what is left, wait for the writes, and return the output.
    pub fn finish(mut self) -> Result<(W, WriteStats), Error> {
        let last = std::mem::take(&mut self.current);
        self.send(last)?;
        let (out, bytes) = self.join()?;
        self.stats.bytes = bytes;
        Ok((out, self.stats))
    }
}
