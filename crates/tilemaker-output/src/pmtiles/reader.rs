//! Minimal in-memory PMTiles v3 reader (used for verification and tests).

use std::path::Path;

use super::{decompress_internal, deserialize_directory, zxy_to_tileid, Entry, Header, HEADER_LEN};
use crate::OutputError;

/// Reads the header from the start of an archive.
pub fn read_header(bytes: &[u8]) -> Result<Header, OutputError> {
    Header::deserialize(bytes)
}

/// An archive loaded fully into memory.
#[derive(Debug)]
pub struct PmtilesReader {
    data: Vec<u8>,
    header: Header,
}

fn find_tile(entries: &[Entry], tile_id: u64) -> Option<Entry> {
    // Last entry with tile_id <= target.
    let idx = entries.partition_point(|e| e.tile_id <= tile_id);
    let e = *entries.get(idx.checked_sub(1)?)?;
    if e.tile_id == tile_id || e.run_length == 0 || tile_id - e.tile_id < u64::from(e.run_length) {
        Some(e)
    } else {
        None
    }
}

impl PmtilesReader {
    /// Loads an archive from disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OutputError> {
        Self::from_bytes(std::fs::read(path)?)
    }

    /// Wraps archive bytes.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, OutputError> {
        let header = read_header(&data)?;
        if data.len() < HEADER_LEN {
            return Err(OutputError::Other("truncated PMTiles archive".into()));
        }
        Ok(PmtilesReader { data, header })
    }

    /// The archive header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    fn slice(&self, offset: u64, len: u64) -> Result<&[u8], OutputError> {
        let start = usize::try_from(offset).map_err(|_| OutputError::Other("offset overflow".into()))?;
        let end = start
            .checked_add(len as usize)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| OutputError::Other("PMTiles range out of bounds".into()))?;
        Ok(&self.data[start..end])
    }

    fn directory(&self, offset: u64, len: u64) -> Result<Vec<Entry>, OutputError> {
        let raw = decompress_internal(self.slice(offset, len)?, self.header.internal_compression)?;
        deserialize_directory(&raw)
    }

    /// The decoded JSON metadata.
    pub fn metadata(&self) -> Result<serde_json::Value, OutputError> {
        let h = &self.header;
        let raw = decompress_internal(
            self.slice(h.json_metadata_offset, h.json_metadata_bytes)?,
            h.internal_compression,
        )?;
        serde_json::from_slice(&raw).map_err(|e| OutputError::Other(e.to_string()))
    }

    /// All tile entries (leaf pointers resolved), in directory order.
    pub fn entries(&self) -> Result<Vec<Entry>, OutputError> {
        let mut out = Vec::new();
        self.collect(self.header.root_dir_offset, self.header.root_dir_bytes, &mut out, 0)?;
        Ok(out)
    }

    fn collect(&self, offset: u64, len: u64, out: &mut Vec<Entry>, depth: u32) -> Result<(), OutputError> {
        if depth > 3 {
            return Err(OutputError::Other("PMTiles directory too deep".into()));
        }
        for e in self.directory(offset, len)? {
            if e.run_length == 0 {
                self.collect(
                    self.header.leaf_dirs_offset + e.offset,
                    u64::from(e.length),
                    out,
                    depth + 1,
                )?;
            } else {
                out.push(e);
            }
        }
        Ok(())
    }

    /// Returns the tile bytes as stored (still tile-compressed), if present.
    pub fn get_tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, OutputError> {
        let id = zxy_to_tileid(z, x, y);
        let (mut offset, mut len) = (self.header.root_dir_offset, self.header.root_dir_bytes);
        for _ in 0..4 {
            let entries = self.directory(offset, len)?;
            match find_tile(&entries, id) {
                Some(e) if e.run_length > 0 => {
                    let data = self.slice(self.header.tile_data_offset + e.offset, u64::from(e.length))?;
                    return Ok(Some(data.to_vec()));
                }
                Some(e) => {
                    offset = self.header.leaf_dirs_offset + e.offset;
                    len = u64::from(e.length);
                }
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    /// Like [`get_tile`](Self::get_tile) but decompresses the tile.
    pub fn get_tile_decompressed(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, OutputError> {
        match self.get_tile(z, x, y)? {
            Some(d) => Ok(Some(decompress_internal(&d, self.header.tile_compression)?)),
            None => Ok(None),
        }
    }
}
