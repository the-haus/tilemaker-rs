//! PMTiles v3 output.
//!
//! See <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md>.
//! Tiles are appended to a temporary data file as they arrive (deduplicated
//! by content); [`PmtilesWriter::finish`] sorts them by Hilbert tile id,
//! builds the directories and writes the final, clustered archive.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{compress, decompress, Compression, OutputError, TileSink, TilesetMetadata};

pub mod reader;

/// Length of the fixed header.
pub const HEADER_LEN: usize = 127;
/// Maximum size of header plus root directory.
const ROOT_LIMIT: usize = 16384 - HEADER_LEN;
/// Compressed tiles up to this size are deduplicated via an exact in-memory map.
const TINY_LEN: usize = 256;
/// The tiny map is cleared when it grows beyond this many entries.
const TINY_MAX_ENTRIES: usize = 100_000;

/// Compression codes used in the header.
pub const COMPRESSION_UNKNOWN: u8 = 0;
/// No compression.
pub const COMPRESSION_NONE: u8 = 1;
/// gzip.
pub const COMPRESSION_GZIP: u8 = 2;
/// Tile type code for Mapbox Vector Tiles.
pub const TILE_TYPE_MVT: u8 = 1;

/// Converts z/x/y to a PMTiles (Hilbert curve) tile id.
///
/// # Panics
/// If `z > 31` or x/y are out of range for the zoom.
pub fn zxy_to_tileid(z: u8, x: u32, y: u32) -> u64 {
    assert!(z <= 31, "tile zoom exceeds 64-bit limit");
    let n: u64 = 1 << z;
    assert!(
        u64::from(x) < n && u64::from(y) < n,
        "tile x/y outside zoom level bounds"
    );
    // Number of tiles on all lower zooms: (4^z - 1) / 3.
    let acc = ((1u64 << (2 * u32::from(z))) - 1) / 3;
    let (mut tx, mut ty) = (u64::from(x), u64::from(y));
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(tx & s > 0);
        let ry = u64::from(ty & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        rotate(s, &mut tx, &mut ty, rx, ry);
        s /= 2;
    }
    acc + d
}

/// Converts a tile id back to `(z, x, y)`.
pub fn tileid_to_zxy(id: u64) -> (u8, u32, u32) {
    let mut acc = 0u64;
    for z in 0u8..32 {
        let num = 1u64 << (2 * u32::from(z));
        if acc + num > id {
            let n = 1u64 << z;
            let mut t = id - acc;
            let (mut tx, mut ty) = (0u64, 0u64);
            let mut s = 1u64;
            while s < n {
                let rx = 1 & (t / 2);
                let ry = 1 & (t ^ rx);
                rotate(s, &mut tx, &mut ty, rx, ry);
                tx += s * rx;
                ty += s * ry;
                t /= 4;
                s *= 2;
            }
            return (z, tx as u32, ty as u32);
        }
        acc += num;
    }
    panic!("tile id exceeds 64-bit limit");
}

/// Hilbert quadrant rotation. Uses wrapping arithmetic to mirror the
/// reference implementation's signed (two's complement) behaviour.
fn rotate(n: u64, x: &mut u64, y: &mut u64, rx: u64, ry: u64) {
    if ry == 0 {
        if rx == 1 {
            *x = (n - 1).wrapping_sub(*x);
            *y = (n - 1).wrapping_sub(*y);
        }
        std::mem::swap(x, y);
    }
}

/// One directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// First tile id covered.
    pub tile_id: u64,
    /// Offset (relative to tile data, or to leaf directories for leaf pointers).
    pub offset: u64,
    /// Length in bytes.
    pub length: u32,
    /// Number of consecutive tile ids sharing this content; 0 = leaf pointer.
    pub run_length: u32,
}

/// The 127-byte PMTiles v3 header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// Root directory offset.
    pub root_dir_offset: u64,
    /// Root directory length.
    pub root_dir_bytes: u64,
    /// JSON metadata offset.
    pub json_metadata_offset: u64,
    /// JSON metadata length.
    pub json_metadata_bytes: u64,
    /// Leaf directories offset.
    pub leaf_dirs_offset: u64,
    /// Leaf directories length.
    pub leaf_dirs_bytes: u64,
    /// Tile data offset.
    pub tile_data_offset: u64,
    /// Tile data length.
    pub tile_data_bytes: u64,
    /// Number of addressed tiles.
    pub addressed_tiles_count: u64,
    /// Number of tile entries (after run-length encoding).
    pub tile_entries_count: u64,
    /// Number of distinct tile contents.
    pub tile_contents_count: u64,
    /// Whether tile data is ordered by tile id.
    pub clustered: bool,
    /// Compression of directories and metadata.
    pub internal_compression: u8,
    /// Compression of tile data.
    pub tile_compression: u8,
    /// Tile type.
    pub tile_type: u8,
    /// Minimum zoom.
    pub min_zoom: u8,
    /// Maximum zoom.
    pub max_zoom: u8,
    /// Minimum longitude * 1e7.
    pub min_lon_e7: i32,
    /// Minimum latitude * 1e7.
    pub min_lat_e7: i32,
    /// Maximum longitude * 1e7.
    pub max_lon_e7: i32,
    /// Maximum latitude * 1e7.
    pub max_lat_e7: i32,
    /// Center zoom.
    pub center_zoom: u8,
    /// Center longitude * 1e7.
    pub center_lon_e7: i32,
    /// Center latitude * 1e7.
    pub center_lat_e7: i32,
}

impl Header {
    /// Serialises the header to its 127-byte form.
    pub fn serialize(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER_LEN);
        b.extend_from_slice(b"PMTiles");
        b.push(3);
        for v in [
            self.root_dir_offset,
            self.root_dir_bytes,
            self.json_metadata_offset,
            self.json_metadata_bytes,
            self.leaf_dirs_offset,
            self.leaf_dirs_bytes,
            self.tile_data_offset,
            self.tile_data_bytes,
            self.addressed_tiles_count,
            self.tile_entries_count,
            self.tile_contents_count,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[
            u8::from(self.clustered),
            self.internal_compression,
            self.tile_compression,
            self.tile_type,
            self.min_zoom,
            self.max_zoom,
        ]);
        for v in [self.min_lon_e7, self.min_lat_e7, self.max_lon_e7, self.max_lat_e7] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(self.center_zoom);
        b.extend_from_slice(&self.center_lon_e7.to_le_bytes());
        b.extend_from_slice(&self.center_lat_e7.to_le_bytes());
        debug_assert_eq!(b.len(), HEADER_LEN);
        b
    }

    /// Parses a header from (at least) the first 127 bytes of an archive.
    pub fn deserialize(b: &[u8]) -> Result<Header, OutputError> {
        if b.len() < HEADER_LEN || &b[..7] != b"PMTiles" {
            return Err(OutputError::Other("not a PMTiles archive".into()));
        }
        if b[7] != 3 {
            return Err(OutputError::Other(format!("unsupported PMTiles version {}", b[7])));
        }
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        let i32_at = |i: usize| i32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        Ok(Header {
            root_dir_offset: u64_at(8),
            root_dir_bytes: u64_at(16),
            json_metadata_offset: u64_at(24),
            json_metadata_bytes: u64_at(32),
            leaf_dirs_offset: u64_at(40),
            leaf_dirs_bytes: u64_at(48),
            tile_data_offset: u64_at(56),
            tile_data_bytes: u64_at(64),
            addressed_tiles_count: u64_at(72),
            tile_entries_count: u64_at(80),
            tile_contents_count: u64_at(88),
            clustered: b[96] == 1,
            internal_compression: b[97],
            tile_compression: b[98],
            tile_type: b[99],
            min_zoom: b[100],
            max_zoom: b[101],
            min_lon_e7: i32_at(102),
            min_lat_e7: i32_at(106),
            max_lon_e7: i32_at(110),
            max_lat_e7: i32_at(114),
            center_zoom: b[118],
            center_lon_e7: i32_at(119),
            center_lat_e7: i32_at(123),
        })
    }
}

fn write_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

/// Serialises a directory (uncompressed).
pub fn serialize_directory(entries: &[Entry]) -> Vec<u8> {
    let mut b = Vec::with_capacity(entries.len() * 6 + 8);
    write_varint(&mut b, entries.len() as u64);
    let mut last = 0;
    for e in entries {
        write_varint(&mut b, e.tile_id - last);
        last = e.tile_id;
    }
    for e in entries {
        write_varint(&mut b, u64::from(e.run_length));
    }
    for e in entries {
        write_varint(&mut b, u64::from(e.length));
    }
    for (i, e) in entries.iter().enumerate() {
        if i > 0 && e.offset == entries[i - 1].offset + u64::from(entries[i - 1].length) {
            write_varint(&mut b, 0);
        } else {
            write_varint(&mut b, e.offset + 1);
        }
    }
    b
}

/// Parses an (uncompressed) directory.
pub fn deserialize_directory(data: &[u8]) -> Result<Vec<Entry>, OutputError> {
    let bad = || OutputError::Other("malformed PMTiles directory".into());
    let mut pos = 0usize;
    let mut next = || -> Result<u64, OutputError> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let byte = *data.get(pos).ok_or_else(bad)?;
            pos += 1;
            if shift >= 64 {
                return Err(bad());
            }
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    };
    let n = next()? as usize;
    if n / 4 > data.len() {
        return Err(bad());
    }
    let mut entries = vec![
        Entry {
            tile_id: 0,
            offset: 0,
            length: 0,
            run_length: 0
        };
        n
    ];
    let mut last = 0u64;
    for e in entries.iter_mut() {
        last = last.checked_add(next()?).ok_or_else(bad)?;
        e.tile_id = last;
    }
    for e in entries.iter_mut() {
        e.run_length = u32::try_from(next()?).map_err(|_| bad())?;
    }
    for e in entries.iter_mut() {
        e.length = u32::try_from(next()?).map_err(|_| bad())?;
    }
    for i in 0..n {
        let v = next()?;
        entries[i].offset = if i > 0 && v == 0 {
            entries[i - 1].offset + u64::from(entries[i - 1].length)
        } else {
            v.checked_sub(1).ok_or_else(bad)?
        };
    }
    Ok(entries)
}

fn gzip(data: &[u8]) -> Vec<u8> {
    compress(data, Compression::Gzip)
}

/// Builds a root directory plus leaf directories with `leaf_size` entries each.
fn build_root_leaves(entries: &[Entry], leaf_size: usize) -> (Vec<u8>, Vec<u8>) {
    let mut root = Vec::new();
    let mut leaves = Vec::new();
    for chunk in entries.chunks(leaf_size) {
        let leaf = gzip(&serialize_directory(chunk));
        root.push(Entry {
            tile_id: chunk[0].tile_id,
            offset: leaves.len() as u64,
            length: leaf.len() as u32,
            run_length: 0,
        });
        leaves.extend_from_slice(&leaf);
    }
    (gzip(&serialize_directory(&root)), leaves)
}

/// Returns `(compressed root, compressed leaves)`, using leaf directories
/// only when the root alone would not fit, as in the reference implementation.
fn make_root_leaves(entries: &[Entry]) -> (Vec<u8>, Vec<u8>) {
    let root = gzip(&serialize_directory(entries));
    if root.len() <= ROOT_LIMIT {
        return (root, Vec::new());
    }
    let mut leaf_size = 4096;
    loop {
        let (root, leaves) = build_root_leaves(entries, leaf_size);
        if root.len() < ROOT_LIMIT {
            return (root, leaves);
        }
        leaf_size *= 2;
    }
}

fn e7(v: f64) -> i32 {
    (v * 10_000_000.0).round() as i32
}

/// Writes a PMTiles v3 archive.
pub struct PmtilesWriter {
    path: PathBuf,
    tmp_path: PathBuf,
    tmp: BufWriter<File>,
    /// Separate read handle on the temp file (for collision checks).
    tmp_reader: Option<File>,
    tmp_len: u64,
    compression: Compression,
    /// `(tile id, content index)` in arrival order.
    tiles: Vec<(u64, u32)>,
    /// `(offset in temp file, length)` per distinct content.
    contents: Vec<(u64, u32)>,
    by_hash: HashMap<(u64, u32), u32>,
    tiny: HashMap<Vec<u8>, u32>,
    finished: bool,
}

impl std::fmt::Debug for PmtilesWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PmtilesWriter")
            .field("path", &self.path)
            .field("compression", &self.compression)
            .field("tiles", &self.tiles.len())
            .field("contents", &self.contents.len())
            .finish()
    }
}

impl PmtilesWriter {
    /// Creates a writer for `path`; tile data is staged in `path + ".tmp"`.
    /// PMTiles has no zlib code, so `Deflate` is written as gzip.
    pub fn create(path: impl AsRef<Path>, compression: Compression) -> Result<Self, OutputError> {
        let path = path.as_ref().to_path_buf();
        let mut tmp_name = path.clone().into_os_string();
        tmp_name.push(".tmp");
        let tmp_path = PathBuf::from(tmp_name);
        let tmp = BufWriter::with_capacity(1 << 20, File::create(&tmp_path)?);
        let compression = match compression {
            Compression::None => Compression::None,
            Compression::Gzip | Compression::Deflate => Compression::Gzip,
        };
        Ok(PmtilesWriter {
            path,
            tmp_path,
            tmp,
            tmp_reader: None,
            tmp_len: 0,
            compression,
            tiles: Vec::new(),
            contents: Vec::new(),
            by_hash: HashMap::new(),
            tiny: HashMap::new(),
            finished: false,
        })
    }

    /// Reads a content blob back from the temp file.
    fn read_content(&mut self, idx: u32) -> Result<Vec<u8>, OutputError> {
        self.tmp.flush()?;
        if self.tmp_reader.is_none() {
            self.tmp_reader = Some(File::open(&self.tmp_path)?);
        }
        let (offset, len) = self.contents[idx as usize];
        let r = self.tmp_reader.as_mut().unwrap();
        r.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0; len as usize];
        r.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Finds an existing identical content, or appends a new one.
    fn intern(&mut self, blob: Vec<u8>) -> Result<u32, OutputError> {
        let tiny = blob.len() <= TINY_LEN;
        if tiny {
            if let Some(&i) = self.tiny.get(&blob) {
                return Ok(i);
            }
        }
        let mut h = DefaultHasher::new();
        blob.hash(&mut h);
        let key = (h.finish(), blob.len() as u32);
        let existing = self.by_hash.get(&key).copied();
        if let Some(i) = existing {
            if self.read_content(i)? == blob {
                if tiny {
                    self.insert_tiny(blob, i);
                }
                return Ok(i);
            }
        }

        let idx = self.contents.len() as u32;
        self.tmp.write_all(&blob)?;
        self.contents.push((self.tmp_len, blob.len() as u32));
        self.tmp_len += blob.len() as u64;
        if existing.is_none() {
            self.by_hash.insert(key, idx);
        }
        if tiny {
            self.insert_tiny(blob, idx);
        }
        Ok(idx)
    }

    fn insert_tiny(&mut self, blob: Vec<u8>, idx: u32) {
        if self.tiny.len() >= TINY_MAX_ENTRIES {
            self.tiny.clear();
        }
        self.tiny.insert(blob, idx);
    }

    fn metadata_json(meta: &TilesetMetadata) -> Value {
        let mut doc = Map::new();
        for (k, v) in &meta.extra {
            doc.insert(k.clone(), v.clone());
        }
        doc.insert("name".into(), json!(meta.name));
        doc.insert("description".into(), json!(meta.description));
        doc.insert("version".into(), json!(meta.version));
        doc.insert("vector_layers".into(), meta.vector_layers());
        Value::Object(doc)
    }
}

impl TileSink for PmtilesWriter {
    fn write_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<(), OutputError> {
        if z > 31 || (u64::from(x) >> z) != 0 || (u64::from(y) >> z) != 0 {
            return Err(OutputError::Other(format!("tile {z}/{x}/{y} out of range")));
        }
        let id = zxy_to_tileid(z, x, y);
        let idx = self.intern(compress(data, self.compression))?;
        self.tiles.push((id, idx));
        Ok(())
    }

    fn finish(&mut self, meta: &TilesetMetadata) -> Result<(), OutputError> {
        if self.finished {
            return Err(OutputError::Other("PMTiles archive already finished".into()));
        }
        self.tmp.flush()?;

        // Sort by tile id; if a tile was written twice the last write wins.
        let mut tiles = std::mem::take(&mut self.tiles);
        tiles.sort_by_key(|t| t.0);
        let mut deduped: Vec<(u64, u32)> = Vec::with_capacity(tiles.len());
        for t in tiles {
            match deduped.last_mut() {
                Some(last) if last.0 == t.0 => *last = t,
                _ => deduped.push(t),
            }
        }

        // Assign clustered offsets in tile-id order and build RLE entries.
        let mut new_offset = vec![u64::MAX; self.contents.len()];
        let mut copy_order: Vec<u32> = Vec::new();
        let mut data_len = 0u64;
        let mut entries: Vec<Entry> = Vec::new();
        for &(id, c) in &deduped {
            let len = self.contents[c as usize].1;
            if new_offset[c as usize] == u64::MAX {
                new_offset[c as usize] = data_len;
                data_len += u64::from(len);
                copy_order.push(c);
            }
            let offset = new_offset[c as usize];
            if let Some(last) = entries.last_mut() {
                if last.offset == offset && last.tile_id + u64::from(last.run_length) == id {
                    last.run_length += 1;
                    continue;
                }
            }
            entries.push(Entry {
                tile_id: id,
                offset,
                length: len,
                run_length: 1,
            });
        }

        let (root, leaves) = make_root_leaves(&entries);
        let metadata = gzip(Self::metadata_json(meta).to_string().as_bytes());

        let bounds = meta.bounds.unwrap_or([-180.0, -85.051129, 180.0, 85.051129]);
        let (clon, clat, czoom) = meta.effective_center().unwrap_or((0.0, 0.0, meta.minzoom));
        let (min_zoom, max_zoom) = match (deduped.first(), deduped.last()) {
            (Some(first), Some(last)) => (tileid_to_zxy(first.0).0, tileid_to_zxy(last.0).0),
            _ => (meta.minzoom, meta.maxzoom),
        };
        let root_off = HEADER_LEN as u64;
        let meta_off = root_off + root.len() as u64;
        let leaf_off = meta_off + metadata.len() as u64;
        let data_off = leaf_off + leaves.len() as u64;
        let header = Header {
            root_dir_offset: root_off,
            root_dir_bytes: root.len() as u64,
            json_metadata_offset: meta_off,
            json_metadata_bytes: metadata.len() as u64,
            leaf_dirs_offset: leaf_off,
            leaf_dirs_bytes: leaves.len() as u64,
            tile_data_offset: data_off,
            tile_data_bytes: data_len,
            addressed_tiles_count: deduped.len() as u64,
            tile_entries_count: entries.len() as u64,
            tile_contents_count: copy_order.len() as u64,
            clustered: true,
            internal_compression: COMPRESSION_GZIP,
            tile_compression: match self.compression {
                Compression::None => COMPRESSION_NONE,
                _ => COMPRESSION_GZIP,
            },
            tile_type: TILE_TYPE_MVT,
            min_zoom: min_zoom.min(meta.minzoom),
            max_zoom: max_zoom.max(meta.maxzoom),
            min_lon_e7: e7(bounds[0]),
            min_lat_e7: e7(bounds[1]),
            max_lon_e7: e7(bounds[2]),
            max_lat_e7: e7(bounds[3]),
            center_zoom: czoom,
            center_lon_e7: e7(clon),
            center_lat_e7: e7(clat),
        };

        let mut out = BufWriter::with_capacity(1 << 20, File::create(&self.path)?);
        out.write_all(&header.serialize())?;
        out.write_all(&root)?;
        out.write_all(&metadata)?;
        out.write_all(&leaves)?;

        // Copy tile contents from the temp file in clustered order.
        let mut src = File::open(&self.tmp_path)?;
        let mut pos = 0u64;
        let mut buf = Vec::new();
        for c in copy_order {
            let (offset, len) = self.contents[c as usize];
            if offset != pos {
                src.seek(SeekFrom::Start(offset))?;
            }
            buf.resize(len as usize, 0);
            src.read_exact(&mut buf)?;
            pos = offset + u64::from(len);
            out.write_all(&buf)?;
        }
        out.flush()?;
        drop(src);

        self.finished = true;
        self.tmp_reader = None;
        self.contents = Vec::new();
        self.by_hash = HashMap::new();
        self.tiny = HashMap::new();
        std::fs::remove_file(&self.tmp_path)?;
        Ok(())
    }

    /// PMTiles archives are written fresh, so there is nothing to merge with.
    fn read_tile(&self, _z: u8, _x: u32, _y: u32) -> Result<Option<Vec<u8>>, OutputError> {
        Ok(None)
    }
}

impl Drop for PmtilesWriter {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::fs::remove_file(&self.tmp_path);
        }
    }
}

/// Decompresses a gzip blob if the given header code says so.
pub(crate) fn decompress_internal(data: &[u8], code: u8) -> Result<Vec<u8>, OutputError> {
    match code {
        COMPRESSION_NONE => Ok(data.to_vec()),
        COMPRESSION_GZIP | COMPRESSION_UNKNOWN => Ok(decompress(data)?),
        c => Err(OutputError::Other(format!("unsupported PMTiles compression {c}"))),
    }
}

#[cfg(test)]
mod tests;
