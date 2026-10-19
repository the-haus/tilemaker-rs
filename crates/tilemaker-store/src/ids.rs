//! Sets of element ids.
//!
//! Ids are split into chunks of 65,536. A chunk holds a sorted array of
//! 16-bit offsets while it has at most 4,096 members and a 8 KiB bitmap
//! beyond that (the Roaring layout), so a sparse set costs about two bytes
//! per id instead of a bit per possible id: the 67 million nodes of the
//! world's boundaries span ids up to 14 billion, a 1.8 GB plain bitmap but
//! about 140 MB here.
//!
//! [`IdSetBuilder`] takes inserts from many threads (chunks are spread over
//! 64 locked shards);
//! [`IdSetBuilder::freeze`] turns it into an [`IdSet`] with lock-free
//! lookups.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const CHUNK_BITS: u32 = 16;
const ARRAY_MAX: usize = 4096;
/// Ids below 2^36 (68 billion) use the chunk table; others a hash map.
const MAX_DENSE: u64 = 1 << 36;
const SHARDS: usize = 64;

enum Pending {
    /// Unsorted offsets, possibly with duplicates.
    Array(Vec<u16>),
    Bitmap(Box<[u64; 1024]>),
}

impl Pending {
    fn insert(&mut self, off: u16) {
        match self {
            Pending::Array(v) => {
                v.push(off);
                if v.len() > 2 * ARRAY_MAX {
                    v.sort_unstable();
                    v.dedup();
                    if v.len() > ARRAY_MAX {
                        let mut bits = Box::new([0u64; 1024]);
                        for &o in v.iter() {
                            bits[(o >> 6) as usize] |= 1 << (o & 63);
                        }
                        *self = Pending::Bitmap(bits);
                    }
                }
            }
            Pending::Bitmap(bits) => bits[(off >> 6) as usize] |= 1 << (off & 63),
        }
    }

    fn freeze(self) -> Container {
        match self {
            Pending::Array(mut v) => {
                v.sort_unstable();
                v.dedup();
                v.shrink_to_fit();
                if v.len() > ARRAY_MAX {
                    let mut bits = Box::new([0u64; 1024]);
                    for &o in &v {
                        bits[(o >> 6) as usize] |= 1 << (o & 63);
                    }
                    Container::Bitmap(bits)
                } else {
                    Container::Array(v.into_boxed_slice())
                }
            }
            Pending::Bitmap(bits) => Container::Bitmap(bits),
        }
    }
}

enum Container {
    Array(Box<[u16]>),
    Bitmap(Box<[u64; 1024]>),
}

impl Container {
    #[inline]
    fn contains(&self, off: u16) -> bool {
        match self {
            Container::Array(a) => a.binary_search(&off).is_ok(),
            Container::Bitmap(b) => b[(off >> 6) as usize] & (1 << (off & 63)) != 0,
        }
    }

    fn any_between(&self, from: u16, to: u16) -> bool {
        match self {
            Container::Array(a) => {
                let i = a.partition_point(|&x| x < from);
                i < a.len() && a[i] <= to
            }
            Container::Bitmap(b) => (from..=to).any(|o| b[(o >> 6) as usize] & (1 << (o & 63)) != 0),
        }
    }

    fn len(&self) -> usize {
        match self {
            Container::Array(a) => a.len(),
            Container::Bitmap(b) => b.iter().map(|w| w.count_ones() as usize).sum(),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Container::Array(a) => a.len() * 2,
            Container::Bitmap(_) => 8192,
        }
    }
}

/// Collects ids from many threads at once.
pub struct IdSetBuilder {
    shards: Box<[Mutex<HashMap<u32, Pending>>]>,
    others: Mutex<Vec<i64>>,
    inserts: AtomicUsize,
}

impl Default for IdSetBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IdSetBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
            others: Mutex::new(Vec::new()),
            inserts: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn dense(id: i64) -> bool {
        id >= 0 && (id as u64) < MAX_DENSE
    }

    /// Add an id.
    #[inline]
    pub fn insert(&self, id: i64) {
        self.insert_all(std::slice::from_ref(&id));
    }

    /// Add many ids (one lock per run of ids in the same chunk).
    pub fn insert_all(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        self.inserts.fetch_add(ids.len(), Ordering::Relaxed);
        let mut i = 0;
        while i < ids.len() {
            let id = ids[i];
            if !Self::dense(id) {
                self.others.lock().unwrap().push(id);
                i += 1;
                continue;
            }
            let c = (id as u64 >> CHUNK_BITS) as u32;
            let mut shard = self.shards[c as usize % SHARDS].lock().unwrap();
            let pending = shard.entry(c).or_insert_with(|| Pending::Array(Vec::new()));
            while i < ids.len() && Self::dense(ids[i]) && (ids[i] as u64 >> CHUNK_BITS) as u32 == c {
                pending.insert(ids[i] as u16);
                i += 1;
            }
        }
    }

    /// Whether nothing was inserted.
    pub fn is_empty(&self) -> bool {
        self.inserts.load(Ordering::Relaxed) == 0
    }

    /// Finish inserting.
    pub fn freeze(self) -> IdSet {
        let mut chunks: Vec<(u32, Container)> = Vec::new();
        for shard in self.shards.into_vec() {
            for (c, p) in shard.into_inner().unwrap() {
                chunks.push((c, p.freeze()));
            }
        }
        let mut table: Vec<Option<Container>> = Vec::new();
        if let Some(max) = chunks.iter().map(|c| c.0).max() {
            table.resize_with(max as usize + 1, || None);
            for (c, container) in chunks {
                table[c as usize] = Some(container);
            }
        }
        let mut others = self.others.into_inner().unwrap();
        others.sort_unstable();
        others.dedup();
        IdSet {
            table: table.into_boxed_slice(),
            others: others.into_boxed_slice(),
        }
    }
}

/// A frozen set of ids (see the module docs).
#[derive(Default)]
pub struct IdSet {
    table: Box<[Option<Container>]>,
    others: Box<[i64]>,
}

impl IdSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `id` is in the set.
    #[inline]
    pub fn contains(&self, id: i64) -> bool {
        if id < 0 || id as u64 >= MAX_DENSE {
            return self.others.binary_search(&id).is_ok();
        }
        match self.table.get((id as u64 >> CHUNK_BITS) as usize) {
            Some(Some(c)) => c.contains(id as u16),
            _ => false,
        }
    }

    /// Whether any id in `lo..=hi` is in the set.
    pub fn any_in_range(&self, lo: i64, hi: i64) -> bool {
        if lo > hi {
            return false;
        }
        if self.others.iter().any(|&o| o >= lo && o <= hi) {
            return true;
        }
        let (lo, hi) = (lo.max(0), hi.min(MAX_DENSE as i64 - 1));
        if lo > hi {
            return false;
        }
        let (c0, c1) = ((lo as u64 >> CHUNK_BITS) as usize, (hi as u64 >> CHUNK_BITS) as usize);
        for c in c0..=c1.min(self.table.len().saturating_sub(1)) {
            let Some(Some(container)) = self.table.get(c) else {
                continue;
            };
            let base = (c as u64) << CHUNK_BITS;
            let from = if c == c0 { (lo as u64 - base) as u16 } else { 0 };
            let to = if c == c1 { (hi as u64 - base) as u16 } else { u16::MAX };
            if container.any_between(from, to) {
                return true;
            }
        }
        false
    }

    /// Number of ids.
    pub fn len(&self) -> usize {
        self.table.iter().flatten().map(Container::len).sum::<usize>() + self.others.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.table.iter().all(Option::is_none) && self.others.is_empty()
    }

    /// Approximate heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        self.table.len() * std::mem::size_of::<Option<Container>>()
            + self.table.iter().flatten().map(Container::bytes).sum::<usize>()
            + self.others.len() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_contains() {
        let b = IdSetBuilder::new();
        assert!(b.is_empty());
        let ids = [0, 1, 63, 64, 65_535, 65_536, 14_274_176_831, -3, i64::MAX];
        for id in ids {
            b.insert(id);
            b.insert(id);
        }
        assert!(!b.is_empty());
        let s = b.freeze();
        for id in ids {
            assert!(s.contains(id), "{id}");
        }
        assert_eq!(s.len(), 9);
        for id in [2, 65_537, 14_274_176_832, -4, 1 << 40] {
            assert!(!s.contains(id), "{id}");
        }
    }

    #[test]
    fn dense_chunks_become_bitmaps() {
        let b = IdSetBuilder::new();
        let ids: Vec<i64> = (0..65_536).step_by(3).collect();
        b.insert_all(&ids);
        b.insert_all(&ids);
        let s = b.freeze();
        assert_eq!(s.len(), ids.len());
        for i in 0..65_536 {
            assert_eq!(s.contains(i), i % 3 == 0);
        }
        assert!(s.heap_bytes() < 10_000);
    }

    #[test]
    fn sparse_sets_are_small() {
        let b = IdSetBuilder::new();
        for i in 0..100_000i64 {
            b.insert(i * 100_003);
        }
        let s = b.freeze();
        assert_eq!(s.len(), 100_000);
        // Two bytes per id plus a 24-byte slot per chunk of the id range.
        assert!(s.heap_bytes() < 100_000 * 2 + 153_000 * 24 + 1024, "{}", s.heap_bytes());
        assert!(s.contains(100_003 * 99_999));
        assert!(!s.contains(100_003 * 99_999 + 1));
    }

    #[test]
    fn range_queries() {
        let b = IdSetBuilder::new();
        b.insert_all(&[5, 70_000, 200_000, 14_274_176_831, -9]);
        let dense: Vec<i64> = (300_000..310_000).collect();
        b.insert_all(&dense);
        let s = b.freeze();
        assert!(s.any_in_range(0, 5));
        assert!(!s.any_in_range(6, 69_999));
        assert!(s.any_in_range(6, 70_000));
        assert!(s.any_in_range(100_000, 250_000));
        assert!(!s.any_in_range(200_001, 299_999));
        assert!(s.any_in_range(305_000, 305_000), "bitmap container");
        assert!(!s.any_in_range(310_000, 1_000_000));
        assert!(s.any_in_range(14_000_000_000, 15_000_000_000));
        assert!(s.any_in_range(-10, -1));
        assert!(!s.any_in_range(10, 4));
    }

    #[test]
    fn concurrent_inserts() {
        let b = IdSetBuilder::new();
        std::thread::scope(|scope| {
            for t in 0..8 {
                let b = &b;
                scope.spawn(move || {
                    for i in 0..10_000i64 {
                        b.insert(i * 8 + t);
                        b.insert(i);
                    }
                });
            }
        });
        let s = b.freeze();
        assert_eq!(s.len(), 80_000);
    }
}
