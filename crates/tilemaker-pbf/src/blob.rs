//! Blobs: the framed, compressed units of a PBF file.

use std::io::Read;

use std::cell::RefCell;

use crate::proto::{Reader, LEN};

thread_local! {
    static LIBDEFLATE: RefCell<libdeflater::Decompressor> = RefCell::new(libdeflater::Decompressor::new());
}
use crate::Error;

/// Largest permitted blob header (per the format spec).
pub const MAX_HEADER_SIZE: usize = 64 * 1024;
/// Largest permitted blob (per the format spec).
pub const MAX_BLOB_SIZE: usize = 32 * 1024 * 1024;

/// What a blob holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobKind {
    /// The file header (`OSMHeader`).
    Header,
    /// Elements (`OSMData`).
    Data,
    /// Anything else; skipped.
    Unknown,
}

/// Where one blob lives in a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobInfo {
    /// Position in the file's sequence of data blobs (header excluded).
    pub index: usize,
    /// Byte offset of the blob body.
    pub offset: u64,
    /// Byte length of the blob body.
    pub size: u32,
    /// Kind.
    pub kind: BlobKind,
}

/// Parse a `BlobHeader`: its type and the size of the blob that follows.
pub(crate) fn parse_blob_header(buf: &[u8]) -> Result<(BlobKind, u32), Error> {
    let mut r = Reader::new(buf);
    let mut kind = BlobKind::Unknown;
    let mut size = None;
    while let Some((field, wire)) = r.field()? {
        match (field, wire) {
            (1, LEN) => {
                kind = match r.bytes()? {
                    b"OSMHeader" => BlobKind::Header,
                    b"OSMData" => BlobKind::Data,
                    _ => BlobKind::Unknown,
                }
            }
            (3, _) => size = Some(r.varint()? as u32),
            _ => r.skip(wire)?,
        }
    }
    let size = size.ok_or(Error::Corrupt("blob header without datasize"))?;
    if size as usize > MAX_BLOB_SIZE {
        return Err(Error::Corrupt("blob larger than 32 MiB"));
    }
    Ok((kind, size))
}

/// Decompress a `Blob` body into `out` (cleared first).
pub fn decompress_blob(buf: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    out.clear();
    let mut r = Reader::new(buf);
    let mut raw_size: Option<usize> = None;
    let mut payload: Option<(u32, &[u8])> = None;
    while let Some((field, wire)) = r.field()? {
        match (field, wire) {
            (2, _) => raw_size = Some(r.varint()? as usize),
            (1 | 3 | 4 | 5 | 6 | 7, LEN) => payload = Some((field, r.bytes()?)),
            _ => r.skip(wire)?,
        }
    }
    let Some((field, data)) = payload else {
        return Err(Error::Corrupt("blob without data"));
    };
    match field {
        1 => out.extend_from_slice(data),
        3 => match raw_size {
            // libdeflate inflates a whole buffer of known size ~2× faster
            // than a streaming inflater.
            Some(size) if size <= MAX_BLOB_SIZE => {
                out.resize(size, 0);
                let n = LIBDEFLATE.with(|d| d.borrow_mut().zlib_decompress(data, out));
                match n {
                    Ok(n) if n == size => {}
                    Ok(_) => return Err(Error::Corrupt("decompressed size differs from raw_size")),
                    Err(e) => return Err(Error::Decompress(format!("{e:?}"))),
                }
            }
            _ => {
                flate2::read::ZlibDecoder::new(data)
                    .take(MAX_BLOB_SIZE as u64 + 1)
                    .read_to_end(out)
                    .map_err(|e| Error::Decompress(e.to_string()))?;
                if out.len() > MAX_BLOB_SIZE {
                    return Err(Error::Corrupt("decompressed blob larger than 32 MiB"));
                }
            }
        },
        4 => return Err(Error::Unsupported("lzma-compressed blobs".into())),
        5 => return Err(Error::Unsupported("bzip2-compressed blobs".into())),
        6 => {
            let size = raw_size.ok_or(Error::Corrupt("lz4 blob without raw_size"))?;
            if size > MAX_BLOB_SIZE {
                return Err(Error::Corrupt("decompressed blob larger than 32 MiB"));
            }
            *out = lz4_flex::block::decompress(data, size).map_err(|e| Error::Decompress(e.to_string()))?;
        }
        7 => {
            let size = raw_size.unwrap_or(MAX_BLOB_SIZE).min(MAX_BLOB_SIZE);
            *out = zstd::bulk::decompress(data, size).map_err(|e| Error::Decompress(e.to_string()))?;
        }
        _ => unreachable!(),
    }
    if let Some(n) = raw_size {
        if n != out.len() {
            return Err(Error::Corrupt("decompressed size differs from raw_size"));
        }
    }
    Ok(())
}

/// The first bytes of a blob's data, decompressed only as far as asked
/// (zlib streams are inflated incrementally; other codecs whole).
pub(crate) struct Prefix<'a> {
    inflater: Option<(flate2::Decompress, &'a [u8])>,
    pub(crate) out: Vec<u8>,
}

impl<'a> Prefix<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.field()? {
            match (field, wire) {
                (3, LEN) => {
                    return Ok(Self {
                        inflater: Some((flate2::Decompress::new(true), r.bytes()?)),
                        out: Vec::new(),
                    })
                }
                (1 | 4 | 5 | 6 | 7, LEN) => break,
                _ => r.skip(wire)?,
            }
        }
        let mut out = Vec::new();
        decompress_blob(buf, &mut out)?;
        Ok(Self { inflater: None, out })
    }

    /// Make at least `n` bytes available (fewer only at the end of the
    /// data); `false` when there are fewer.
    pub(crate) fn ensure(&mut self, n: usize) -> Result<bool, Error> {
        let Some((z, data)) = &mut self.inflater else {
            return Ok(self.out.len() >= n);
        };
        while self.out.len() < n {
            let want = n.max(self.out.len() * 2).clamp(4096, MAX_BLOB_SIZE);
            self.out.reserve(want - self.out.len());
            let before = z.total_out();
            let consumed = z.total_in() as usize;
            let status = z
                .decompress_vec(&data[consumed..], &mut self.out, flate2::FlushDecompress::None)
                .map_err(|e| Error::Decompress(e.to_string()))?;
            if status == flate2::Status::StreamEnd || z.total_out() == before {
                return Ok(self.out.len() >= n);
            }
        }
        Ok(true)
    }
}

/// Reads blobs one after another from any byte stream (a file, stdin, a
/// socket). For random access use [`crate::PbfFile`].
pub struct BlobReader<R> {
    inner: R,
    data_index: usize,
    offset: u64,
}

impl<R: Read> BlobReader<R> {
    /// Wrap a reader positioned at the start of a PBF stream.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            data_index: 0,
            offset: 0,
        }
    }

    /// The next blob's kind and compressed body, or `None` at the end.
    pub fn next_blob(&mut self) -> Result<Option<(BlobInfo, Vec<u8>)>, Error> {
        let mut len = [0u8; 4];
        if !read_exact_or_eof(&mut self.inner, &mut len)? {
            return Ok(None);
        }
        let header_len = u32::from_be_bytes(len) as usize;
        if header_len > MAX_HEADER_SIZE {
            return Err(Error::Corrupt("blob header larger than 64 KiB"));
        }
        let mut header = vec![0u8; header_len];
        self.inner.read_exact(&mut header).map_err(truncated)?;
        let (kind, size) = parse_blob_header(&header)?;
        let mut body = vec![0u8; size as usize];
        self.inner.read_exact(&mut body).map_err(truncated)?;
        let offset = self.offset + 4 + header_len as u64;
        self.offset = offset + size as u64;
        let index = if kind == BlobKind::Data {
            self.data_index += 1;
            self.data_index - 1
        } else {
            usize::MAX
        };
        Ok(Some((
            BlobInfo {
                index,
                offset,
                size,
                kind,
            },
            body,
        )))
    }
}

impl<R: Read> Iterator for BlobReader<R> {
    type Item = Result<(BlobInfo, Vec<u8>), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_blob().transpose()
    }
}

fn truncated(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        Error::Corrupt("file truncated inside a blob")
    } else {
        Error::Io(e)
    }
}

/// Fill `buf`; `false` on a clean EOF before the first byte.
pub(crate) fn read_exact_or_eof(r: &mut impl Read, buf: &mut [u8]) -> Result<bool, Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(Error::Corrupt("file truncated inside a blob length")),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(true)
}
