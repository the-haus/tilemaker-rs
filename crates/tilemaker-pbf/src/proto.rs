//! A minimal protobuf wire-format reader (just what OSM PBF needs).

use crate::Error;

/// Wire types.
pub(crate) const VARINT: u8 = 0;
pub(crate) const FIXED64: u8 = 1;
pub(crate) const LEN: u8 = 2;
pub(crate) const FIXED32: u8 = 5;

/// A cursor over one message's bytes.
#[derive(Clone, Copy)]
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes consumed.
    #[inline]
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    #[inline(always)]
    pub(crate) fn varint(&mut self) -> Result<u64, Error> {
        // Fast path: one byte, or a whole varint's worth of bytes left (no
        // per-byte bounds checks).
        if let Some(rest) = self.buf.get(self.pos..self.pos + 10) {
            let b0 = rest[0];
            if b0 < 0x80 {
                self.pos += 1;
                return Ok(b0 as u64);
            }
            let mut result = (b0 & 0x7f) as u64;
            for (i, &b) in rest.iter().enumerate().skip(1) {
                result |= ((b & 0x7f) as u64) << (7 * i);
                if b < 0x80 {
                    self.pos += i + 1;
                    return Ok(result);
                }
            }
            return Err(Error::Corrupt("varint too long"));
        }
        self.varint_slow()
    }

    #[cold]
    fn varint_slow(&mut self) -> Result<u64, Error> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let Some(&b) = self.buf.get(self.pos) else {
                return Err(Error::Corrupt("truncated varint"));
            };
            self.pos += 1;
            if shift < 64 {
                result |= ((b & 0x7f) as u64) << shift;
            }
            if b & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift > 70 {
                return Err(Error::Corrupt("varint too long"));
            }
        }
    }

    /// The next field's number and wire type, or `None` at the end.
    #[inline]
    pub(crate) fn field(&mut self) -> Result<Option<(u32, u8)>, Error> {
        if self.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        Ok(Some(((key >> 3) as u32, (key & 7) as u8)))
    }

    #[inline]
    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], Error> {
        let len = self.varint()? as usize;
        let end = self
            .pos
            .checked_add(len)
            .filter(|&e| e <= self.buf.len())
            .ok_or(Error::Corrupt("length-delimited field overruns message"))?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    /// The next length-delimited field's bytes, without consuming them.
    #[inline]
    pub(crate) fn peek_bytes(&self) -> Result<&'a [u8], Error> {
        let mut r = *self;
        r.bytes()
    }

    pub(crate) fn skip(&mut self, wire: u8) -> Result<(), Error> {
        match wire {
            VARINT => {
                self.varint()?;
            }
            FIXED64 => self.advance(8)?,
            LEN => {
                self.bytes()?;
            }
            FIXED32 => self.advance(4)?,
            _ => return Err(Error::Corrupt("unsupported wire type")),
        }
        Ok(())
    }

    fn advance(&mut self, n: usize) -> Result<(), Error> {
        if self.pos + n > self.buf.len() {
            return Err(Error::Corrupt("truncated fixed-width field"));
        }
        self.pos += n;
        Ok(())
    }

    #[inline]
    pub(crate) fn int64(&mut self) -> Result<i64, Error> {
        Ok(self.varint()? as i64)
    }

    #[inline]
    pub(crate) fn sint64(&mut self) -> Result<i64, Error> {
        Ok(zigzag(self.varint()?))
    }
}

#[inline]
pub(crate) fn zigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Iterate a packed repeated varint field.
pub(crate) struct Packed<'a>(Reader<'a>);

impl<'a> Packed<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self(Reader::new(buf))
    }
}

impl Iterator for Packed<'_> {
    type Item = Result<u64, Error>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.varint())
        }
    }
}

/// Read a repeated varint field that may be packed (LEN) or not (VARINT),
/// calling `f` for each value.
#[inline]
pub(crate) fn each_varint(r: &mut Reader<'_>, wire: u8, mut f: impl FnMut(u64)) -> Result<(), Error> {
    match wire {
        LEN => {
            for v in Packed::new(r.bytes()?) {
                f(v?);
            }
        }
        VARINT => f(r.varint()?),
        _ => return Err(Error::Corrupt("unexpected wire type for repeated varint")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints() {
        let mut r = Reader::new(&[0x96, 0x01, 0x00, 0xff, 0xff, 0xff, 0xff, 0x0f]);
        assert_eq!(r.varint().unwrap(), 150);
        assert_eq!(r.varint().unwrap(), 0);
        assert_eq!(r.varint().unwrap(), u32::MAX as u64);
        assert!(r.varint().is_err());
    }

    #[test]
    fn zigzags() {
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(1), -1);
        assert_eq!(zigzag(2), 1);
        assert_eq!(zigzag(3), -2);
        assert_eq!(zigzag(u64::MAX), i64::MIN);
    }

    #[test]
    fn overrun_is_an_error() {
        let mut r = Reader::new(&[0x05, 0x01]);
        assert!(r.bytes().is_err());
    }
}
