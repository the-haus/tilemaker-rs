//! Minimal protobuf wire-format helpers used by the encoder and decoder.

use crate::DecodeError;

/// Protobuf wire types.
pub(crate) const WIRE_VARINT: u32 = 0;
pub(crate) const WIRE_FIXED64: u32 = 1;
pub(crate) const WIRE_LEN: u32 = 2;
pub(crate) const WIRE_FIXED32: u32 = 5;

pub(crate) fn write_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

pub(crate) fn write_tag(buf: &mut Vec<u8>, field: u32, wire: u32) {
    write_varint(buf, u64::from((field << 3) | wire));
}

pub(crate) fn write_varint_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    write_tag(buf, field, WIRE_VARINT);
    write_varint(buf, v);
}

pub(crate) fn write_bytes_field(buf: &mut Vec<u8>, field: u32, data: &[u8]) {
    write_tag(buf, field, WIRE_LEN);
    write_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

/// Writes a packed repeated uint32 field. Nothing is written for an empty slice.
pub(crate) fn write_packed_u32(buf: &mut Vec<u8>, field: u32, values: &[u32]) {
    if values.is_empty() {
        return;
    }
    let mut packed = Vec::with_capacity(values.len() * 2);
    for &v in values {
        write_varint(&mut packed, u64::from(v));
    }
    write_bytes_field(buf, field, &packed);
}

#[inline]
pub(crate) fn zigzag32(n: i32) -> u32 {
    ((n << 1) ^ (n >> 31)) as u32
}

#[inline]
pub(crate) fn unzigzag32(n: u32) -> i32 {
    ((n >> 1) as i32) ^ -((n & 1) as i32)
}

#[inline]
pub(crate) fn zigzag64(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

#[inline]
pub(crate) fn unzigzag64(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

/// A field value read from a protobuf message.
pub(crate) enum FieldValue<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

/// Sequential reader over the fields of one protobuf message.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub(crate) fn read_varint(&mut self) -> Result<u64, DecodeError> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *self.data.get(self.pos).ok_or(DecodeError::UnexpectedEof)?;
            self.pos += 1;
            if shift >= 64 {
                return Err(DecodeError::InvalidVarint);
            }
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::UnexpectedEof)?;
        let slice = self.data.get(self.pos..end).ok_or(DecodeError::UnexpectedEof)?;
        self.pos = end;
        Ok(slice)
    }

    /// Returns the next `(field number, value)`, or `None` at end of message.
    pub(crate) fn next_field(&mut self) -> Result<Option<(u32, FieldValue<'a>)>, DecodeError> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }
        let key = self.read_varint()?;
        let field = (key >> 3) as u32;
        let value = match (key & 7) as u32 {
            WIRE_VARINT => FieldValue::Varint(self.read_varint()?),
            WIRE_FIXED64 => {
                let b = self.take(8)?;
                FieldValue::Fixed64(u64::from_le_bytes(b.try_into().unwrap()))
            }
            WIRE_LEN => {
                let len = self.read_varint()? as usize;
                FieldValue::Bytes(self.take(len)?)
            }
            WIRE_FIXED32 => {
                let b = self.take(4)?;
                FieldValue::Fixed32(u32::from_le_bytes(b.try_into().unwrap()))
            }
            w => return Err(DecodeError::UnsupportedWireType(w)),
        };
        Ok(Some((field, value)))
    }
}

/// Decodes a packed repeated uint32 field.
pub(crate) fn read_packed_u32(data: &[u8]) -> Result<Vec<u32>, DecodeError> {
    let mut r = Reader::new(data);
    let mut out = Vec::with_capacity(data.len());
    while r.pos < data.len() {
        out.push(r.read_varint()? as u32);
    }
    Ok(out)
}
