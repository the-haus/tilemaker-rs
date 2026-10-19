//! Vector tile decoder.

use std::fmt;

use crate::pbf::{read_packed_u32, unzigzag32, unzigzag64, FieldValue, Reader};
use crate::{GeomType, Value, CMD_CLOSE_PATH, CMD_LINE_TO, CMD_MOVE_TO};

/// Error returned by [`decode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The buffer ended in the middle of a field.
    UnexpectedEof,
    /// A varint was longer than 10 bytes.
    InvalidVarint,
    /// A wire type this decoder does not understand (groups).
    UnsupportedWireType(u32),
    /// A string field was not valid UTF-8.
    InvalidUtf8,
    /// A field had an unexpected wire type.
    UnexpectedWireType {
        /// Field number.
        field: u32,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnexpectedEof => write!(f, "unexpected end of data"),
            DecodeError::InvalidVarint => write!(f, "invalid varint"),
            DecodeError::UnsupportedWireType(w) => write!(f, "unsupported wire type {w}"),
            DecodeError::InvalidUtf8 => write!(f, "invalid UTF-8 in string"),
            DecodeError::UnexpectedWireType { field } => {
                write!(f, "unexpected wire type for field {field}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// A decoded tile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedTile {
    /// Layers in file order.
    pub layers: Vec<DecodedLayer>,
}

/// A decoded layer.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedLayer {
    /// Layer name.
    pub name: String,
    /// Layer version (defaults to 1 when absent).
    pub version: u32,
    /// Layer extent (defaults to 4096 when absent).
    pub extent: u32,
    /// Key table.
    pub keys: Vec<String>,
    /// Value table.
    pub values: Vec<Value>,
    /// Features in file order.
    pub features: Vec<DecodedFeature>,
}

/// A decoded feature; geometry is kept in its encoded command form.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedFeature {
    /// Feature id, if present.
    pub id: Option<u64>,
    /// Geometry type.
    pub geom_type: GeomType,
    /// Alternating key/value indices into the layer tables.
    pub tags: Vec<u32>,
    /// Raw geometry command stream.
    pub geometry: Vec<u32>,
}

impl DecodedFeature {
    /// Resolves the tags against `layer`'s key and value tables. Tags with
    /// out-of-range indices are skipped.
    pub fn properties<'a>(&'a self, layer: &'a DecodedLayer) -> Vec<(&'a str, &'a Value)> {
        self.tags
            .chunks_exact(2)
            .filter_map(|kv| {
                let k = layer.keys.get(kv[0] as usize)?;
                let v = layer.values.get(kv[1] as usize)?;
                Some((k.as_str(), v))
            })
            .collect()
    }

    /// Decodes the geometry into absolute tile coordinates: one `Vec` per
    /// MoveTo command (a multipoint MoveTo with count n yields one part with
    /// n points). ClosePath repeats the part's first point.
    pub fn decode_geometry(&self) -> Vec<Vec<(i32, i32)>> {
        let g = &self.geometry;
        let mut parts: Vec<Vec<(i32, i32)>> = Vec::new();
        let (mut x, mut y) = (0i32, 0i32);
        let mut i = 0;
        while i < g.len() {
            let id = g[i] & 7;
            let count = (g[i] >> 3) as usize;
            i += 1;
            match id {
                CMD_MOVE_TO | CMD_LINE_TO => {
                    if id == CMD_MOVE_TO || parts.is_empty() {
                        parts.push(Vec::with_capacity(count));
                    }
                    let part = parts.last_mut().unwrap();
                    for _ in 0..count {
                        if i + 1 >= g.len() {
                            return parts;
                        }
                        x = x.wrapping_add(unzigzag32(g[i]));
                        y = y.wrapping_add(unzigzag32(g[i + 1]));
                        i += 2;
                        part.push((x, y));
                    }
                }
                CMD_CLOSE_PATH => {
                    if let Some(part) = parts.last_mut() {
                        if let Some(&first) = part.first() {
                            part.push(first);
                        }
                    }
                }
                _ => return parts,
            }
        }
        parts
    }
}

fn bytes<'a>(field: u32, v: FieldValue<'a>) -> Result<&'a [u8], DecodeError> {
    match v {
        FieldValue::Bytes(b) => Ok(b),
        _ => Err(DecodeError::UnexpectedWireType { field }),
    }
}

fn varint(field: u32, v: FieldValue<'_>) -> Result<u64, DecodeError> {
    match v {
        FieldValue::Varint(n) => Ok(n),
        _ => Err(DecodeError::UnexpectedWireType { field }),
    }
}

fn string(field: u32, v: FieldValue<'_>) -> Result<String, DecodeError> {
    let b = bytes(field, v)?;
    String::from_utf8(b.to_vec()).map_err(|_| DecodeError::InvalidUtf8)
}

/// Decodes a vector tile. Unknown fields are skipped.
pub fn decode(data: &[u8]) -> Result<DecodedTile, DecodeError> {
    let mut tile = DecodedTile::default();
    let mut r = Reader::new(data);
    while let Some((field, v)) = r.next_field()? {
        if field == 3 {
            tile.layers.push(decode_layer(bytes(field, v)?)?);
        }
    }
    Ok(tile)
}

fn decode_layer(data: &[u8]) -> Result<DecodedLayer, DecodeError> {
    let mut layer = DecodedLayer {
        name: String::new(),
        version: 1,
        extent: 4096,
        keys: Vec::new(),
        values: Vec::new(),
        features: Vec::new(),
    };
    let mut r = Reader::new(data);
    while let Some((field, v)) = r.next_field()? {
        match field {
            15 => layer.version = varint(field, v)? as u32,
            1 => layer.name = string(field, v)?,
            2 => layer.features.push(decode_feature(bytes(field, v)?)?),
            3 => layer.keys.push(string(field, v)?),
            4 => layer.values.push(decode_value(bytes(field, v)?)?),
            5 => layer.extent = varint(field, v)? as u32,
            _ => {}
        }
    }
    Ok(layer)
}

fn decode_feature(data: &[u8]) -> Result<DecodedFeature, DecodeError> {
    let mut f = DecodedFeature {
        id: None,
        geom_type: GeomType::Unknown,
        tags: Vec::new(),
        geometry: Vec::new(),
    };
    let mut r = Reader::new(data);
    while let Some((field, v)) = r.next_field()? {
        match (field, v) {
            (1, v) => f.id = Some(varint(field, v)?),
            (2, FieldValue::Bytes(b)) => f.tags.extend(read_packed_u32(b)?),
            (2, FieldValue::Varint(n)) => f.tags.push(n as u32),
            (3, v) => f.geom_type = GeomType::from_u32(varint(field, v)? as u32),
            (4, FieldValue::Bytes(b)) => f.geometry.extend(read_packed_u32(b)?),
            (4, FieldValue::Varint(n)) => f.geometry.push(n as u32),
            _ => {}
        }
    }
    Ok(f)
}

fn decode_value(data: &[u8]) -> Result<Value, DecodeError> {
    let mut r = Reader::new(data);
    let mut value = None;
    while let Some((field, v)) = r.next_field()? {
        value = Some(match (field, v) {
            (1, v) => Value::String(string(field, v)?),
            (2, FieldValue::Fixed32(b)) => Value::Float(f32::from_bits(b)),
            (3, FieldValue::Fixed64(b)) => Value::Double(f64::from_bits(b)),
            (4, FieldValue::Varint(n)) => Value::Int(n as i64),
            (5, FieldValue::Varint(n)) => Value::UInt(n),
            (6, FieldValue::Varint(n)) => Value::SInt(unzigzag64(n)),
            (7, FieldValue::Varint(n)) => Value::Bool(n != 0),
            (2..=7, _) => return Err(DecodeError::UnexpectedWireType { field }),
            _ => continue,
        });
    }
    // An empty value message is unusual; treat it as an empty string.
    Ok(value.unwrap_or(Value::String(String::new())))
}
