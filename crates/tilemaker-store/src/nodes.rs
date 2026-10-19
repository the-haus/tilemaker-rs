//! Node locations.
//!
//! The in-memory store keeps each block's nodes sorted, in groups of 64:
//! one full id per group, the rest as varint deltas, and the locations as
//! fixed (lat, lon) pairs. That is about ten bytes a node, and a lookup is a
//! binary search plus at most 63 varint decodes.
//!
//! The dense file store maps `id × 8` bytes of a sparse file into memory, as
//! osmium's `dense_mmap_array` does: constant-time lookups and no per-node
//! overhead, best when a large share of a planet's nodes is needed.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use memmap2::MmapMut;

use crate::StoreError;

/// A WGS84 location at 1e-7 degrees.
pub type Location = (i32, i32);

const GROUP: usize = 64;

/// One block's worth of nodes.
struct Chunk {
    first_id: i64,
    last_id: i64,
    /// Per group: (first id, byte offset of the group's deltas).
    groups: Vec<(i64, u32)>,
    deltas: Vec<u8>,
    locs: Vec<Location>,
}

impl Chunk {
    fn build(entries: &[(i64, Location)]) -> Self {
        let mut groups = Vec::with_capacity(entries.len() / GROUP + 1);
        let mut deltas = Vec::with_capacity(entries.len() * 2);
        let mut locs = Vec::with_capacity(entries.len());
        for (gi, group) in entries.chunks(GROUP).enumerate() {
            let _ = gi;
            groups.push((group[0].0, deltas.len() as u32));
            let mut prev = group[0].0;
            for (k, &(id, loc)) in group.iter().enumerate() {
                if k > 0 {
                    put_varint(&mut deltas, (id - prev) as u64);
                    prev = id;
                }
                locs.push(loc);
            }
        }
        deltas.shrink_to_fit();
        Self {
            first_id: entries[0].0,
            last_id: entries[entries.len() - 1].0,
            groups,
            deltas,
            locs,
        }
    }

    fn get(&self, id: i64) -> Option<Location> {
        let g = match self.groups.binary_search_by(|(first, _)| first.cmp(&id)) {
            Ok(g) => return Some(self.locs[g * GROUP]),
            Err(0) => return None,
            Err(g) => g - 1,
        };
        let end = self.locs.len().min((g + 1) * GROUP);
        let mut pos = self.groups[g].1 as usize;
        let mut cur = self.groups[g].0;
        for i in g * GROUP + 1..end {
            let (d, n) = get_varint(&self.deltas[pos..]);
            pos += n;
            cur += d as i64;
            if cur == id {
                return Some(self.locs[i]);
            }
            if cur > id {
                return None;
            }
        }
        None
    }

    fn entries(&self) -> impl Iterator<Item = (i64, Location)> + '_ {
        let mut pos = 0usize;
        let mut cur = 0i64;
        self.locs.iter().enumerate().map(move |(i, &loc)| {
            if i % GROUP == 0 {
                cur = self.groups[i / GROUP].0;
                pos = self.groups[i / GROUP].1 as usize;
            } else {
                let (d, n) = get_varint(&self.deltas[pos..]);
                pos += n;
                cur += d as i64;
            }
            (cur, loc)
        })
    }

    fn bytes(&self) -> usize {
        self.groups.len() * 16 + self.deltas.len() + self.locs.len() * 8
    }
}

#[inline]
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
fn get_varint(buf: &[u8]) -> (u64, usize) {
    let mut v = 0u64;
    let mut shift = 0;
    for (i, &b) in buf.iter().enumerate() {
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return (v, i + 1);
        }
        shift += 7;
    }
    (v, buf.len())
}

/// Latitudes are stored offset so that an all-zero slot means "absent".
const LAT_BIAS: i64 = 1_000_000_000;
/// The dense file covers ids below 2^35 (34 billion; the planet is at 14).
const DENSE_MAX_ID: u64 = 1 << 35;

struct DenseFile {
    map: MmapMut,
    path: PathBuf,
    remove_on_drop: bool,
    _file: File,
}

impl Drop for DenseFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

enum BuilderKind {
    Memory(Mutex<Vec<Chunk>>),
    Dense(DenseFile),
}

/// Collects node locations, possibly from many threads at once.
pub struct NodeStoreBuilder {
    kind: BuilderKind,
}

impl NodeStoreBuilder {
    /// Insert one batch of nodes (typically one block's). The batch is
    /// sorted in place.
    pub fn insert(&self, batch: &mut Vec<(i64, Location)>) -> Result<(), StoreError> {
        if batch.is_empty() {
            return Ok(());
        }
        match &self.kind {
            BuilderKind::Memory(chunks) => {
                batch.sort_unstable_by_key(|e| e.0);
                batch.dedup_by_key(|e| e.0);
                let chunk = Chunk::build(batch);
                chunks.lock().unwrap().push(chunk);
            }
            BuilderKind::Dense(d) => {
                let base = d.map.as_ptr() as *mut u64;
                for &(id, (lat, lon)) in batch.iter() {
                    if id < 0 || id as u64 >= DENSE_MAX_ID {
                        return Err(StoreError::OutOfRange(id));
                    }
                    let v = (((lat as i64 + LAT_BIAS) as u64) << 32) | (lon as u32 as u64);
                    // SAFETY: in bounds (checked above); distinct ids are
                    // distinct slots, and a slot written twice gets the same
                    // node's location, so racing writers cannot tear a value
                    // into something meaningful-but-wrong beyond what a
                    // duplicate node in the input would do. Readers only run
                    // after `freeze`.
                    unsafe { base.add(id as usize).write_unaligned(v.to_le()) };
                }
            }
        }
        Ok(())
    }

    /// Finish inserting and get a store for lookups.
    pub fn freeze(self) -> NodeStore {
        match self.kind {
            BuilderKind::Memory(chunks) => {
                let mut chunks = chunks.into_inner().unwrap();
                chunks.sort_unstable_by_key(|c| c.first_id);
                let overlapping = chunks.windows(2).any(|w| w[0].last_id >= w[1].first_id);
                if overlapping {
                    // Unsorted input: merge everything into sorted chunks.
                    let mut all: Vec<(i64, Location)> = chunks.iter().flat_map(|c| c.entries()).collect();
                    all.sort_unstable_by_key(|e| e.0);
                    all.dedup_by_key(|e| e.0);
                    chunks = all.chunks(8000).map(Chunk::build).collect();
                }
                let starts = chunks.iter().map(|c| c.first_id).collect();
                NodeStore {
                    kind: StoreKind::Memory { chunks, starts },
                }
            }
            BuilderKind::Dense(d) => NodeStore {
                kind: StoreKind::Dense(d),
            },
        }
    }
}

enum StoreKind {
    Memory { chunks: Vec<Chunk>, starts: Vec<i64> },
    Dense(DenseFile),
}

/// Node id → location lookups.
pub struct NodeStore {
    kind: StoreKind,
}

impl NodeStore {
    /// A builder for an in-memory store.
    pub fn memory() -> NodeStoreBuilder {
        NodeStoreBuilder {
            kind: BuilderKind::Memory(Mutex::new(Vec::new())),
        }
    }

    /// A builder for a dense store in a (sparse) file at `path`, removed
    /// when the store is dropped. Needs a 64-bit platform and a filesystem
    /// with sparse files (APFS, ext4, XFS, NTFS); disk use is about 8 bytes
    /// per page-sized run of ids actually touched.
    pub fn dense_file(path: impl AsRef<Path>) -> Result<NodeStoreBuilder, StoreError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        file.set_len(DENSE_MAX_ID * 8)?;
        // SAFETY: the file is private to this store (created and truncated
        // above, removed on drop); nothing else maps or resizes it.
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(NodeStoreBuilder {
            kind: BuilderKind::Dense(DenseFile {
                map,
                path,
                remove_on_drop: true,
                _file: file,
            }),
        })
    }

    /// The location of node `id`.
    #[inline]
    pub fn get(&self, id: i64) -> Option<Location> {
        match &self.kind {
            StoreKind::Memory { chunks, starts } => {
                let i = match starts.binary_search(&id) {
                    Ok(i) => i,
                    Err(0) => return None,
                    Err(i) => i - 1,
                };
                let c = &chunks[i];
                if id > c.last_id {
                    return None;
                }
                c.get(id)
            }
            StoreKind::Dense(d) => {
                if id < 0 || id as u64 >= DENSE_MAX_ID {
                    return None;
                }
                let at = id as usize * 8;
                let v = u64::from_le_bytes(d.map[at..at + 8].try_into().unwrap());
                if v == 0 {
                    return None;
                }
                let lat = ((v >> 32) as i64 - LAT_BIAS) as i32;
                let lon = v as u32 as i32;
                Some((lat, lon))
            }
        }
    }

    /// Number of nodes held (in-memory stores only; 0 for dense files).
    pub fn len(&self) -> usize {
        match &self.kind {
            StoreKind::Memory { chunks, .. } => chunks.iter().map(|c| c.locs.len()).sum(),
            StoreKind::Dense(_) => 0,
        }
    }

    /// Whether the store is empty (in-memory stores only).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Approximate heap bytes used (in-memory stores only).
    pub fn heap_bytes(&self) -> usize {
        match &self.kind {
            StoreKind::Memory { chunks, starts } => chunks.iter().map(Chunk::bytes).sum::<usize>() + starts.len() * 8,
            StoreKind::Dense(_) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(builder: NodeStoreBuilder, ids: &[i64]) -> NodeStore {
        for chunk in ids.chunks(1000) {
            let mut batch: Vec<_> = chunk
                .iter()
                .map(|&id| (id, ((id % 1_800_000_000) as i32 - 900_000_000, -(id as i32))))
                .collect();
            builder.insert(&mut batch).unwrap();
        }
        builder.freeze()
    }

    fn check(store: &NodeStore, ids: &[i64]) {
        for &id in ids {
            assert_eq!(
                store.get(id),
                Some(((id % 1_800_000_000) as i32 - 900_000_000, -(id as i32))),
                "{id}"
            );
        }
    }

    #[test]
    fn memory_sorted_and_sparse() {
        let ids: Vec<i64> = (0..50_000).map(|i| i * 3 + 1).collect();
        let store = fill(NodeStore::memory(), &ids);
        check(&store, &ids);
        assert_eq!(store.get(0), None);
        assert_eq!(store.get(2), None);
        assert_eq!(store.get(150_001), None);
        assert_eq!(store.len(), 50_000);
        assert!(store.heap_bytes() < 50_000 * 11, "{}", store.heap_bytes());
    }

    #[test]
    fn memory_unsorted_batches_merge() {
        let mut ids: Vec<i64> = (0..20_000).map(|i| (i * 7919) % 100_003).collect();
        ids.dedup();
        let store = fill(NodeStore::memory(), &ids);
        check(&store, &ids);
        assert_eq!(store.get(100_003), None);
    }

    #[test]
    fn memory_large_gaps_and_big_ids() {
        let ids = vec![1, 2, 1_000_000, 5_000_000_000, 14_274_176_831];
        let store = fill(NodeStore::memory(), &ids);
        check(&store, &ids);
        assert_eq!(store.get(3), None);
    }

    #[test]
    fn dense_file_store() {
        let path = std::env::temp_dir().join(format!("tilemaker-dense-{}.bin", std::process::id()));
        let ids = vec![0, 1, 77, 1_000_000, 14_274_176_831];
        let store = fill(NodeStore::dense_file(&path).unwrap(), &ids);
        check(&store, &ids);
        assert_eq!(store.get(2), None);
        assert_eq!(store.get(-1), None);
        // Location (−90°, 0) round-trips (not confused with "absent").
        let b = NodeStore::dense_file(path.with_extension("2")).unwrap();
        b.insert(&mut vec![(5, (-900_000_000, 0))]).unwrap();
        let s = b.freeze();
        assert_eq!(s.get(5), Some((-900_000_000, 0)));
        drop(store);
        assert!(!path.exists());
    }
}
