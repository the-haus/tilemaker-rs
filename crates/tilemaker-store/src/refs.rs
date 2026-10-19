//! Way node ids, varint-delta encoded: lets a relation-only export resolve
//! member ways from the node store without reading the ways again.

use std::sync::Mutex;

struct Chunk {
    /// (way id, byte offset), sorted by id.
    index: Vec<(i64, u32)>,
    data: Vec<u8>,
}

/// One block's ways being encoded.
#[derive(Default)]
pub struct RefBatch {
    index: Vec<(i64, u32)>,
    data: Vec<u8>,
}

impl RefBatch {
    /// An empty batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a way's node ids.
    pub fn push(&mut self, id: i64, refs: &[i64]) {
        self.index.push((id, self.data.len() as u32));
        put_varint(&mut self.data, refs.len() as u64);
        let mut prev = 0i64;
        for &r in refs {
            put_varint(&mut self.data, zz(r.wrapping_sub(prev)));
            prev = r;
        }
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

/// Collects way node ids from many threads.
#[derive(Default)]
pub struct RefStoreBuilder {
    chunks: Mutex<Vec<Chunk>>,
}

impl RefStoreBuilder {
    /// A new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a batch.
    pub fn insert(&self, mut batch: RefBatch) {
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
    pub fn freeze(self) -> RefStore {
        let mut chunks = self.chunks.into_inner().unwrap();
        chunks.sort_unstable_by_key(|c| c.index[0].0);
        let sorted = chunks
            .windows(2)
            .all(|w| w[0].index.last().unwrap().0 < w[1].index[0].0);
        let mut global = Vec::new();
        if !sorted {
            for (c, chunk) in chunks.iter().enumerate() {
                global.extend(chunk.index.iter().map(|&(id, at)| (id, c as u32, at)));
            }
            // Stable: of duplicate ids the earliest chunk wins.
            global.sort_by_key(|e| e.0);
            global.dedup_by_key(|e| e.0);
        }
        RefStore { chunks, global, sorted }
    }
}

/// Way id → node ids.
pub struct RefStore {
    chunks: Vec<Chunk>,
    /// For unsorted input (chunk id ranges overlap): every way's
    /// (id, chunk, offset), sorted by id, so lookups stay a binary search.
    global: Vec<(i64, u32, u32)>,
    sorted: bool,
}

impl RefStore {
    /// Decode way `id`'s node ids into `out` (cleared first); `false` if the
    /// way is not stored.
    pub fn get_into(&self, id: i64, out: &mut Vec<i64>) -> bool {
        out.clear();
        let decode = |c: &Chunk, at: u32, out: &mut Vec<i64>| {
            let mut pos = at as usize;
            let (n, k) = get_varint(&c.data[pos..]);
            pos += k;
            let mut cur = 0i64;
            out.reserve(n as usize);
            for _ in 0..n {
                let (d, k) = get_varint(&c.data[pos..]);
                pos += k;
                cur = cur.wrapping_add(unzz(d));
                out.push(cur);
            }
        };
        let find = |c: &Chunk| c.index.binary_search_by(|e| e.0.cmp(&id)).ok().map(|j| c.index[j].1);
        if self.sorted {
            let i = match self.chunks.binary_search_by(|c| c.index[0].0.cmp(&id)) {
                Ok(i) => i,
                Err(0) => return false,
                Err(i) => i - 1,
            };
            if let Some(at) = find(&self.chunks[i]) {
                decode(&self.chunks[i], at, out);
                return true;
            }
            false
        } else {
            match self.global.binary_search_by(|e| e.0.cmp(&id)) {
                Ok(j) => {
                    let (_, c, at) = self.global[j];
                    decode(&self.chunks[c as usize], at, out);
                    true
                }
                Err(_) => false,
            }
        }
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
        self.chunks
            .iter()
            .map(|c| c.index.len() * 16 + c.data.len())
            .sum::<usize>()
            + self.global.len() * 16
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
        let b = RefStoreBuilder::new();
        let mut want = Vec::new();
        for block in 0..5i64 {
            let mut batch = RefBatch::new();
            for k in 0..200i64 {
                let id = block * 10_000 + k * 7;
                let refs: Vec<i64> = (0..(k % 11)).map(|j| 14_000_000_000 - j * 1_000_003 + k).collect();
                batch.push(id, &refs);
                want.push((id, refs));
            }
            b.insert(batch);
        }
        let s = b.freeze();
        assert_eq!(s.len(), 1000);
        let mut out = Vec::new();
        for (id, refs) in want {
            assert!(s.get_into(id, &mut out));
            assert_eq!(out, refs);
        }
        assert!(!s.get_into(1, &mut out));
        assert!(!s.get_into(-5, &mut out));
    }

    #[test]
    fn negative_ids_and_unsorted_batches() {
        let b = RefStoreBuilder::new();
        let mut x = RefBatch::new();
        x.push(30, &[-1, -2, 5]);
        x.push(10, &[i64::MAX, i64::MIN]);
        b.insert(x);
        let mut y = RefBatch::new();
        y.push(20, &[]);
        b.insert(y);
        let s = b.freeze();
        let mut out = Vec::new();
        assert!(s.get_into(10, &mut out));
        assert_eq!(out, vec![i64::MAX, i64::MIN]);
        assert!(s.get_into(20, &mut out) && out.is_empty());
        assert!(s.get_into(30, &mut out));
        assert_eq!(out, vec![-1, -2, 5]);
    }
}
