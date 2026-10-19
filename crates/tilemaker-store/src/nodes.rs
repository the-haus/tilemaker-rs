//! Node locations.
//!
//! The in-memory store keeps each block's nodes sorted, in groups of 16:
//! one full id per group, the rest as varint deltas, and the locations as
//! fixed (lat, lon) pairs. That is about ten bytes a node, and a lookup is a
//! binary search plus at most 15 varint decodes.
//!
//! The dense file store maps `id × 8` bytes of a sparse file into memory, as
//! osmium's `dense_mmap_array` does: constant-time lookups and no per-node
//! overhead, best when a large share of a planet's nodes is needed.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use memmap2::MmapMut;

use crate::StoreError;

/// A WGS84 location at 1e-7 degrees.
pub type Location = (i32, i32);

const GROUP: usize = 16;

/// One block's worth of nodes.
struct Chunk {
    first_id: i64,
    last_id: i64,
    /// Per group: (first id, byte offset of the group's deltas).
    /// Per group: the first id, and where its deltas start.
    group_ids: Vec<i64>,
    group_offsets: Vec<u32>,
    deltas: Vec<u8>,
    locs: Vec<Location>,
}

impl Chunk {
    fn build(entries: &[(i64, Location)]) -> Self {
        let mut group_ids = Vec::with_capacity(entries.len() / GROUP + 1);
        let mut group_offsets = Vec::with_capacity(entries.len() / GROUP + 1);
        let mut deltas = Vec::with_capacity(entries.len() * 2);
        let mut locs = Vec::with_capacity(entries.len());
        for group in entries.chunks(GROUP) {
            group_ids.push(group[0].0);
            group_offsets.push(deltas.len() as u32);
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
            group_ids,
            group_offsets,
            deltas,
            locs,
        }
    }

    fn get(&self, id: i64) -> Option<Location> {
        let g = match self.group_ids.binary_search(&id) {
            Ok(g) => return Some(self.locs[g * GROUP]),
            Err(0) => return None,
            Err(g) => g - 1,
        };
        let end = self.locs.len().min((g + 1) * GROUP);
        let mut pos = self.group_offsets[g] as usize;
        let mut cur = self.group_ids[g];
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
                cur = self.group_ids[i / GROUP];
                pos = self.group_offsets[i / GROUP] as usize;
            } else {
                let (d, n) = get_varint(&self.deltas[pos..]);
                pos += n;
                cur += d as i64;
            }
            (cur, loc)
        })
    }

    fn bytes(&self) -> usize {
        self.group_ids.len() * 12 + self.deltas.len() + self.locs.len() * 8
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

/// Mark a file sparse, so that sizing it does not allocate disk space.
/// (Unix filesystems with sparse files need nothing.)
#[cfg(windows)]
fn make_sparse(file: &File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let mut returned = 0u32;
    // SAFETY: a valid handle for the file's lifetime; no buffers.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn make_sparse(_file: &File) -> std::io::Result<()> {
    Ok(())
}

/// Latitudes are stored offset so that an all-zero slot means "absent".
const LAT_BIAS: i64 = 1_000_000_000;
/// The dense file covers ids below 2^35 (34 billion; the planet is at 14).
const DENSE_MAX_ID: u64 = 1 << 35;

struct DenseFile {
    /// The mapping viewed as one atomic slot per id. Points into `_map`,
    /// which lives as long as this struct and is never touched otherwise.
    slots: *const AtomicU64,
    /// Ids outside the slots' range (negative, or ≥ 2^35).
    others: Mutex<HashMap<i64, Location>>,
    /// Locations written (duplicates count twice).
    written: std::sync::atomic::AtomicUsize,
    _map: MmapMut,
    /// Where the file is, when it could not be unlinked while open
    /// (Windows): removed on drop.
    remove_on_drop: Option<PathBuf>,
    _file: File,
}

// SAFETY: `slots` points into the mapping owned by the same struct and is
// only accessed through atomics.
unsafe impl Send for DenseFile {}
unsafe impl Sync for DenseFile {}

impl DenseFile {
    #[inline]
    fn slots(&self) -> &[AtomicU64] {
        // SAFETY: `slots` was derived from the whole (page-aligned) mapping
        // through `&mut`, and the mapping outlives `self`.
        unsafe { std::slice::from_raw_parts(self.slots, DENSE_MAX_ID as usize) }
    }

    fn set(&self, id: i64, loc: Location) {
        if id < 0 || id as u64 >= DENSE_MAX_ID {
            self.others.lock().unwrap().insert(id, loc);
            return;
        }
        let v = (((loc.0 as i64 + LAT_BIAS) as u64) << 32) | (loc.1 as u32 as u64);
        self.slots()[id as usize].store(v, Ordering::Relaxed);
    }

    fn get(&self, id: i64) -> Option<Location> {
        if id < 0 || id as u64 >= DENSE_MAX_ID {
            return self.others.lock().unwrap().get(&id).copied();
        }
        let v = self.slots()[id as usize].load(Ordering::Relaxed);
        if v == 0 {
            return None;
        }
        Some((((v >> 32) as i64 - LAT_BIAS) as i32, v as u32 as i32))
    }
}

impl Drop for DenseFile {
    fn drop(&mut self) {
        if let Some(p) = self.remove_on_drop.take() {
            let _ = std::fs::remove_file(p);
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
                for &(id, loc) in batch.iter() {
                    d.set(id, loc);
                }
                d.written.fetch_add(batch.len(), Ordering::Relaxed);
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

    /// A builder for a dense store in a new (sparse) file at `path`, which
    /// must not exist yet. On Unix the file is unlinked as soon as it is
    /// mapped (nothing is left behind, even after a crash); elsewhere it is
    /// removed when the store is dropped. Needs a 64-bit platform and a
    /// filesystem with sparse files (APFS, ext4, XFS, NTFS); disk use is
    /// about one page per run of ids actually touched.
    pub fn dense_file(path: impl AsRef<Path>) -> Result<NodeStoreBuilder, StoreError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        let setup = || -> Result<MmapMut, StoreError> {
            make_sparse(&file)?;
            file.set_len(DENSE_MAX_ID * 8)?;
            // SAFETY: the file was just created by us; nothing else maps
            // or resizes it.
            Ok(unsafe { MmapMut::map_mut(&file)? })
        };
        let mut map = match setup() {
            Ok(m) => m,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        };
        let remove_on_drop = if cfg!(unix) {
            std::fs::remove_file(&path)?;
            None
        } else {
            Some(path)
        };
        let slots = map.as_mut_ptr() as *const AtomicU64;
        Ok(NodeStoreBuilder {
            kind: BuilderKind::Dense(DenseFile {
                slots,
                others: Mutex::new(HashMap::new()),
                written: std::sync::atomic::AtomicUsize::new(0),
                _map: map,
                remove_on_drop,
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
            StoreKind::Dense(d) => d.get(id),
        }
    }

    /// Number of nodes held (for a dense file: locations written).
    pub fn len(&self) -> usize {
        match &self.kind {
            StoreKind::Memory { chunks, .. } => chunks.iter().map(|c| c.locs.len()).sum(),
            StoreKind::Dense(d) => d.written.load(Ordering::Relaxed),
        }
    }

    /// Whether the store is empty.
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
        let _ = std::fs::remove_file(&path);
        let ids = vec![0, 1, 77, 1_000_000, 14_274_176_831, -5, 1 << 40];
        let store = fill(NodeStore::dense_file(&path).unwrap(), &ids);
        check(&store, &ids);
        assert_eq!(store.get(2), None);
        assert_eq!(store.get(-1), None);
        // An existing file is never overwritten.
        std::fs::write(path.with_extension("keep"), b"precious").unwrap();
        assert!(NodeStore::dense_file(path.with_extension("keep")).is_err());
        assert_eq!(std::fs::read(path.with_extension("keep")).unwrap(), b"precious");
        let _ = std::fs::remove_file(path.with_extension("keep"));
        // Location (−90°, 0) round-trips (not confused with "absent").
        let _ = std::fs::remove_file(path.with_extension("2"));
        let b = NodeStore::dense_file(path.with_extension("2")).unwrap();
        b.insert(&mut vec![(5, (-900_000_000, 0))]).unwrap();
        let s = b.freeze();
        assert_eq!(s.get(5), Some((-900_000_000, 0)));
        drop(store);
        assert!(!path.exists());
    }
}
