//! Way geometries (node locations), varint-delta encoded.

use std::sync::Mutex;

use crate::nodes::Location;

struct Chunk {
    /// (way id, byte offset) sorted by id.
    index: Vec<(i64, u32)>,
    data: Vec<u8>,
}

/// Collects way geometries, possibly from many threads at once.
#[derive(Default)]
pub struct WayStoreBuilder {
    chunks: Mutex<Vec<Chunk>>,
}

/// A batch of ways being encoded (one per block, typically).
#[derive(Default)]
pub struct WayBatch {
    index: Vec<(i64, u32)>,
    data: Vec<u8>,
}

impl WayBatch {
    /// An empty batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a way's locations.
    pub fn push(&mut self, id: i64, locs: impl ExactSizeIterator<Item = Location>) {
        self.index.push((id, self.data.len() as u32));
        put_varint(&mut self.data, locs.len() as u64);
        let (mut plat, mut plon) = (0i64, 0i64);
        for (lat, lon) in locs {
            put_varint(&mut self.data, zz(lat as i64 - plat));
            put_varint(&mut self.data, zz(lon as i64 - plon));
            plat = lat as i64;
            plon = lon as i64;
        }
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

impl WayStoreBuilder {
    /// A new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a batch.
    pub fn insert(&self, mut batch: WayBatch) {
        if batch.is_empty() {
            return;
        }
        if !batch.index.windows(2).all(|w| w[0].0 < w[1].0) {
            batch.index.sort_unstable_by_key(|e| e.0);
            batch.index.dedup_by_key(|e| e.0);
        }
        batch.data.shrink_to_fit();
        self.chunks.lock().unwrap().push(Chunk {
            index: batch.index,
            data: batch.data,
        });
    }

    /// Finish inserting.
    pub fn freeze(self) -> WayStore {
        let mut chunks = self.chunks.into_inner().unwrap();
        chunks.sort_unstable_by_key(|c| c.index[0].0);
        let sorted = chunks
            .windows(2)
            .all(|w| w[0].index.last().unwrap().0 < w[1].index[0].0);
        WayStore { chunks, sorted }
    }
}

/// Way id → locations.
pub struct WayStore {
    chunks: Vec<Chunk>,
    /// Whether chunk id ranges are disjoint (one binary search suffices).
    sorted: bool,
}

impl WayStore {
    /// An empty store.
    pub fn empty() -> Self {
        Self {
            chunks: Vec::new(),
            sorted: true,
        }
    }

    /// Decode way `id`'s locations into `out` (cleared first); `false` if
    /// the way is not stored.
    pub fn get_into(&self, id: i64, out: &mut Vec<Location>) -> bool {
        out.clear();
        let decode = |c: &Chunk, at: u32, out: &mut Vec<Location>| {
            let mut pos = at as usize;
            let (n, k) = get_varint(&c.data[pos..]);
            pos += k;
            let (mut lat, mut lon) = (0i64, 0i64);
            out.reserve(n as usize);
            for _ in 0..n {
                let (d, k) = get_varint(&c.data[pos..]);
                pos += k;
                lat += unzz(d);
                let (d, k) = get_varint(&c.data[pos..]);
                pos += k;
                lon += unzz(d);
                out.push((lat as i32, lon as i32));
            }
        };
        if self.sorted {
            let i = match self.chunks.binary_search_by(|c| c.index[0].0.cmp(&id)) {
                Ok(i) => i,
                Err(0) => return false,
                Err(i) => i - 1,
            };
            let c = &self.chunks[i];
            if let Ok(j) = c.index.binary_search_by(|e| e.0.cmp(&id)) {
                decode(c, c.index[j].1, out);
                return true;
            }
            false
        } else {
            for c in &self.chunks {
                if let Ok(j) = c.index.binary_search_by(|e| e.0.cmp(&id)) {
                    decode(c, c.index[j].1, out);
                    return true;
                }
            }
            false
        }
    }

    /// Way `id`'s locations.
    pub fn get(&self, id: i64) -> Option<Vec<Location>> {
        let mut v = Vec::new();
        self.get_into(id, &mut v).then_some(v)
    }

    /// Number of ways stored.
    pub fn len(&self) -> usize {
        self.chunks.iter().map(|c| c.index.len()).sum()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Approximate heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        self.chunks.iter().map(|c| c.index.len() * 16 + c.data.len()).sum()
    }
}

#[inline]
fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
fn unzz(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let b = WayStoreBuilder::new();
        for block in 0..10i64 {
            let mut batch = WayBatch::new();
            for w in 0..100i64 {
                let id = block * 1000 + w * 3;
                let locs: Vec<Location> = (0..(w % 9) as i32 + 1)
                    .map(|k| (k * 10 - 900_000_000, -k * 7 + 1_799_999_999))
                    .collect();
                batch.push(id, locs.into_iter());
            }
            b.insert(batch);
        }
        let s = b.freeze();
        assert_eq!(s.len(), 1000);
        for block in 0..10i64 {
            for w in 0..100i64 {
                let id = block * 1000 + w * 3;
                let want: Vec<Location> = (0..(w % 9) as i32 + 1)
                    .map(|k| (k * 10 - 900_000_000, -k * 7 + 1_799_999_999))
                    .collect();
                assert_eq!(s.get(id), Some(want));
            }
        }
        assert_eq!(s.get(1), None);
        assert_eq!(s.get(99_999), None);
        assert!(s.heap_bytes() < 1000 * 16 + 5 * 1000 * 6);
    }

    #[test]
    fn overlapping_chunks() {
        let b = WayStoreBuilder::new();
        let mut b1 = WayBatch::new();
        b1.push(10, [(1, 1)].into_iter());
        b1.push(30, [(3, 3)].into_iter());
        let mut b2 = WayBatch::new();
        b2.push(20, [(2, 2)].into_iter());
        b.insert(b1);
        b.insert(b2);
        let s = b.freeze();
        assert_eq!(s.get(20), Some(vec![(2, 2)]));
        assert_eq!(s.get(30), Some(vec![(3, 3)]));
    }
}
