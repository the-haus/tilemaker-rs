//! gzip / zlib helpers.

use std::io::{Read, Write};

use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};

/// Tile compression policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// Store tiles uncompressed.
    None,
    /// gzip stream.
    Gzip,
    /// zlib stream (tilemaker's `compress_string` with `gzip=false`).
    Deflate,
}

/// Compresses `data` at the default level.
pub fn compress(data: &[u8], c: Compression) -> Vec<u8> {
    let level = flate2::Compression::default();
    // Writing into a Vec cannot fail.
    match c {
        Compression::None => data.to_vec(),
        Compression::Gzip => {
            let mut e = GzEncoder::new(Vec::with_capacity(data.len() / 2 + 32), level);
            e.write_all(data).expect("in-memory write");
            e.finish().expect("in-memory write")
        }
        Compression::Deflate => {
            let mut e = ZlibEncoder::new(Vec::with_capacity(data.len() / 2 + 16), level);
            e.write_all(data).expect("in-memory write");
            e.finish().expect("in-memory write")
        }
    }
}

/// Decompresses gzip (`1f 8b`) or zlib (`78 xx`) data; anything else is
/// returned unchanged.
pub fn decompress(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 3);
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        GzDecoder::new(data).read_to_end(&mut out)?;
    } else if data.len() >= 2 && data[0] & 0x0f == 8 && (u16::from(data[0]) << 8 | u16::from(data[1])) % 31 == 0 {
        ZlibDecoder::new(data).read_to_end(&mut out)?;
    } else {
        return Ok(data.to_vec());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_modes() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 7) as u8).collect();
        for c in [Compression::None, Compression::Gzip, Compression::Deflate] {
            let z = compress(&data, c);
            match c {
                Compression::Gzip => assert_eq!(&z[..2], &[0x1f, 0x8b]),
                Compression::Deflate => assert_eq!(z[0], 0x78),
                Compression::None => assert_eq!(z, data),
            }
            assert_eq!(decompress(&z).unwrap(), data);
        }
    }

    #[test]
    fn raw_mvt_passthrough() {
        // MVT tiles start with 0x1a (field 3, length-delimited).
        let raw = [0x1a, 0x03, 0x01, 0x02, 0x03];
        assert_eq!(decompress(&raw).unwrap(), raw);
    }
}
