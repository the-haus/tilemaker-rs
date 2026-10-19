//! Random access to a PBF file: blob index, header, parallel block decoding.

use std::fs::File;
use std::io::{BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::blob::{decompress_blob, parse_blob_header, read_exact_or_eof, BlobInfo, BlobKind, MAX_HEADER_SIZE};
use crate::block::{BlockKinds, DecodeOptions, PrimitiveBlock};
use crate::header::Header;
use crate::parallel::{ordered_map, OrderedMap};
use crate::Error;

/// An open PBF file with its blob index.
#[derive(Clone)]
pub struct PbfFile {
    path: PathBuf,
    file: Arc<File>,
    header: Header,
    blobs: Arc<Vec<BlobInfo>>,
    len: u64,
    kinds: Arc<Mutex<Vec<Option<BlockKinds>>>>,
}

/// Which blocks hold which element types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Blocks that may contain nodes.
    pub nodes: Range<usize>,
    /// Blocks that may contain ways.
    pub ways: Range<usize>,
    /// Blocks that may contain relations.
    pub relations: Range<usize>,
}

impl PbfFile {
    /// Open a file and index its blobs (reads only the small blob headers).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let len = file.metadata()?.len();
        let mut r = BufReader::with_capacity(64 * 1024, &file);
        let mut blobs = Vec::new();
        let mut header = None;
        let mut offset = 0u64;
        let mut lenbuf = [0u8; 4];
        let mut hbuf = Vec::new();
        loop {
            if !read_exact_or_eof(&mut r, &mut lenbuf)? {
                break;
            }
            let hlen = u32::from_be_bytes(lenbuf) as usize;
            if hlen > MAX_HEADER_SIZE {
                return Err(Error::Corrupt("blob header larger than 64 KiB"));
            }
            hbuf.resize(hlen, 0);
            r.read_exact(&mut hbuf)
                .map_err(|_| Error::Corrupt("file truncated inside a blob header"))?;
            let (kind, size) = parse_blob_header(&hbuf)?;
            let body = offset + 4 + hlen as u64;
            if body + size as u64 > len {
                return Err(Error::Corrupt("file truncated inside a blob"));
            }
            match kind {
                BlobKind::Header if header.is_none() => {
                    let mut raw = vec![0u8; size as usize];
                    r.read_exact(&mut raw)?;
                    let mut out = Vec::new();
                    decompress_blob(&raw, &mut out)?;
                    header = Some(Header::decode(&out)?);
                }
                BlobKind::Data => {
                    blobs.push(BlobInfo {
                        index: blobs.len(),
                        offset: body,
                        size,
                        kind,
                    });
                    r.seek_relative(size as i64)?;
                }
                _ => r.seek_relative(size as i64)?,
            }
            offset = body + size as u64;
        }
        let header = header.ok_or(Error::Corrupt("no OSMHeader blob"))?;
        header.check_supported()?;
        let n = blobs.len();
        Ok(Self {
            path,
            file: Arc::new(file),
            header,
            blobs: Arc::new(blobs),
            len,
            kinds: Arc::new(Mutex::new(vec![None; n])),
        })
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's size in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the file holds no data blobs.
    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }

    /// The header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The data blobs, in file order.
    pub fn blobs(&self) -> &[BlobInfo] {
        &self.blobs
    }

    /// Read a blob's compressed body.
    pub fn read_blob(&self, info: &BlobInfo) -> Result<Vec<u8>, Error> {
        let mut buf = vec![0u8; info.size as usize];
        read_at(&self.file, &mut buf, info.offset)?;
        Ok(buf)
    }

    /// Read and decode one block.
    pub fn decode_block(&self, index: usize, opts: DecodeOptions) -> Result<PrimitiveBlock, Error> {
        let info = self
            .blobs
            .get(index)
            .ok_or(Error::Corrupt("block index out of range"))?;
        let raw = self.read_blob(info)?;
        let mut out = Vec::new();
        decompress_blob(&raw, &mut out)?;
        let block = PrimitiveBlock::decode(&out, opts)?;
        self.kinds.lock().unwrap()[index] = Some(block.kinds());
        Ok(block)
    }

    /// What element types a block holds (decoding it the first time).
    pub fn block_kinds(&self, index: usize) -> Result<BlockKinds, Error> {
        if let Some(k) = self.kinds.lock().unwrap()[index] {
            return Ok(k);
        }
        let opts = DecodeOptions {
            nodes: false,
            ways: false,
            relations: false,
            metadata: false,
        };
        Ok(self.decode_block(index, opts)?.kinds())
    }

    /// Decode `range` of blocks on `threads` workers, yielding
    /// `(block index, block)` in file order.
    pub fn blocks(
        &self,
        range: Range<usize>,
        opts: DecodeOptions,
        threads: usize,
    ) -> OrderedMap<Result<(usize, PrimitiveBlock), Error>> {
        self.map_blocks(range, opts, threads, |i, b| Ok((i, b)))
    }

    /// Decode `range` of blocks and run `f` on each, on `threads` workers;
    /// results arrive in file order. Heavy per-block work belongs in `f`.
    pub fn map_blocks<R, F>(
        &self,
        range: Range<usize>,
        opts: DecodeOptions,
        threads: usize,
        f: F,
    ) -> OrderedMap<Result<R, Error>>
    where
        R: Send + 'static,
        F: Fn(usize, PrimitiveBlock) -> Result<R, Error> + Send + Sync + 'static,
    {
        let range = range.start.min(self.blobs.len())..range.end.min(self.blobs.len());
        self.map_block_list(range.collect(), opts, threads, f)
    }

    /// Like [`map_blocks`](Self::map_blocks) for a list of block indices
    /// (in the order given).
    pub fn map_block_list<R, F>(
        &self,
        blocks: Vec<usize>,
        opts: DecodeOptions,
        threads: usize,
        f: F,
    ) -> OrderedMap<Result<R, Error>>
    where
        R: Send + 'static,
        F: Fn(usize, PrimitiveBlock) -> Result<R, Error> + Send + Sync + 'static,
    {
        let n = self.blobs.len();
        let reader = self.clone();
        let raws = blocks.into_iter().filter(move |&i| i < n).map(move |i| {
            let info = reader.blobs[i];
            (i, reader.read_blob(&info))
        });
        let kinds = self.kinds.clone();
        ordered_map(raws, threads, threads * 2 + 2, move |(i, raw)| {
            let raw = raw?;
            let mut out = Vec::new();
            decompress_blob(&raw, &mut out)?;
            let block = PrimitiveBlock::decode(&out, opts)?;
            kinds.lock().unwrap()[i] = Some(block.kinds());
            f(i, block)
        })
    }

    /// Which blocks hold nodes, ways and relations. For files sorted by type
    /// then id this binary-searches the boundaries (decoding a few dozen
    /// blocks); otherwise every range is the whole file.
    pub fn layout(&self) -> Result<Layout, Error> {
        let n = self.blobs.len();
        if !self.header.is_sorted() || n == 0 {
            return Ok(Layout {
                nodes: 0..n,
                ways: 0..n,
                relations: 0..n,
            });
        }
        // rank: 0 nodes, 1 ways, 2 relations; per block the min and max
        // present. Both are non-decreasing in a sorted file, except that a
        // block with no elements (or only changesets) has no rank: if the
        // search meets one, fall back to reading everything everywhere.
        let rank = |k: BlockKinds| -> Option<(u8, u8)> {
            let present = [k.nodes, k.ways, k.relations];
            let min = present.iter().position(|&p| p)? as u8;
            let max = present.iter().rposition(|&p| p)? as u8;
            Some((min, max))
        };
        let whole = Layout {
            nodes: 0..n,
            ways: 0..n,
            relations: 0..n,
        };
        let first = |pred: &dyn Fn((u8, u8)) -> bool| -> Result<Option<usize>, Error> {
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let Some(r) = rank(self.block_kinds(mid)?) else {
                    return Ok(None);
                };
                if pred(r) {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            Ok(Some(lo))
        };
        let (Some(nodes_end), Some(ways_start), Some(ways_end), Some(rels_start)) = (
            first(&|(min, _)| min > 0)?,
            first(&|(_, max)| max >= 1)?,
            first(&|(min, _)| min > 1)?,
            first(&|(_, max)| max >= 2)?,
        ) else {
            return Ok(whole);
        };
        Ok(Layout {
            nodes: 0..nodes_end,
            ways: ways_start..ways_end.max(ways_start),
            relations: rels_start..n,
        })
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut f = file.try_clone()?;
    f.seek(SeekFrom::Start(offset))?;
    f.read_exact(buf)
}
