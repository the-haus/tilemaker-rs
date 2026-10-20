//! GeoJSON text, written directly rather than through `serde_json`
//! values: coordinates print from their 1e-7 fixed point (exact, at most 7
//! decimals), and a block's features serialise on the worker that made
//! them ([`write_geojson_seq`]).
//!
//! A feature is `{"type":"Feature","id":"way/1","properties":{…},
//! "bbox":[…],"geometry":{…}}`: properties are the tags plus `@type` and
//! `@id`, metadata as `@version`, `@timestamp` (ISO 8601), `@changeset`,
//! `@uid`, `@user`, and computed values as `@area`, `@length`,
//! `@centroid`, `@labelPoint`, `@memberOf` (`[{"id","role","tags"}]`);
//! `bbox` is there when computed. A generated property wins over a tag of
//! the same name.

use std::io::Write;

use crate::engine::{export_features_with, Event, ExportStats, FeatureOptions, Flow, Progress, Weight};
use crate::record::{Feature, Geometry, GeometryType};
use crate::{ExportError, PbfFile};

/// Options for GeoJSON output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GeoJsonOptions {
    /// Start each feature with an ASCII record separator (RFC 8142
    /// GeoJSON text sequences, as `osmium export -f geojsonseq` by default).
    pub record_separator: bool,
}

fn string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let esc: &[u8] = match b {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0..=0x1f => b"",
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..i]);
        if esc.is_empty() {
            let _ = write!(out, "\\u{:04x}", b);
        } else {
            out.extend_from_slice(esc);
        }
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

/// A coordinate in degrees, rounded to 7 decimals, without trailing zeros.
fn coord(out: &mut Vec<u8>, v: f64) {
    if !v.is_finite() {
        out.extend_from_slice(b"null");
        return;
    }
    let e7 = (v * 1e7).round() as i64;
    if e7 < 0 {
        out.push(b'-');
    }
    let a = e7.unsigned_abs();
    let _ = write!(out, "{}", a / 10_000_000);
    let mut frac = a % 10_000_000;
    if frac != 0 {
        let mut digits = 7;
        while frac.is_multiple_of(10) {
            frac /= 10;
            digits -= 1;
        }
        let _ = write!(out, ".{:0width$}", frac, width = digits);
    }
}

/// A measure (m or m²), to the millimetre.
fn measure(out: &mut Vec<u8>, v: f64) {
    if v.is_finite() {
        let _ = write!(out, "{}", (v * 1000.0).round() / 1000.0);
    } else {
        out.extend_from_slice(b"null");
    }
}

fn position(out: &mut Vec<u8>, c: &[f64]) {
    out.push(b'[');
    coord(out, c[0]);
    out.push(b',');
    coord(out, c[1]);
    out.push(b']');
}

fn positions(out: &mut Vec<u8>, c: &[f64]) {
    out.push(b'[');
    for (i, p) in c.chunks_exact(2).enumerate() {
        if i > 0 {
            out.push(b',');
        }
        position(out, p);
    }
    out.push(b']');
}

/// A geometry object.
pub fn write_geometry(g: &Geometry, out: &mut Vec<u8>) {
    out.extend_from_slice(b"{\"type\":\"");
    out.extend_from_slice(g.kind.as_str().as_bytes());
    out.extend_from_slice(b"\",\"coordinates\":");
    let line = |out: &mut Vec<u8>, s: usize, e: usize| positions(out, &g.coords[2 * s..2 * e]);
    match g.kind {
        GeometryType::Point => position(out, &g.coords),
        GeometryType::LineString => positions(out, &g.coords),
        GeometryType::MultiLineString => {
            out.push(b'[');
            let mut start = 0;
            for (i, &end) in g.parts.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                line(out, start, end as usize);
                start = end as usize;
            }
            out.push(b']');
        }
        GeometryType::MultiPolygon => {
            out.push(b'[');
            let (mut ring, mut start) = (0usize, 0usize);
            for (p, &ring_end) in g.polygons.iter().enumerate() {
                if p > 0 {
                    out.push(b',');
                }
                out.push(b'[');
                for r in ring..ring_end as usize {
                    if r > ring {
                        out.push(b',');
                    }
                    let end = g.parts[r] as usize;
                    line(out, start, end);
                    start = end;
                }
                out.push(b']');
                ring = ring_end as usize;
            }
            out.push(b']');
        }
    }
    out.push(b'}');
}

/// Seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso8601(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Writes one property value.
type Writer<'a> = dyn Fn(&mut Vec<u8>) + 'a;

/// One feature (no newline).
pub fn write_feature(f: &Feature, out: &mut Vec<u8>) {
    let kind = f.kind.as_str();
    let x = &f.extras;
    // Generated properties, which win over tags of the same name.
    let mut generated: Vec<(&str, Box<Writer<'_>>)> = vec![
        ("@type", Box::new(move |o: &mut Vec<u8>| string(o, kind))),
        (
            "@id",
            Box::new(|o: &mut Vec<u8>| {
                let _ = write!(o, "{}", f.id);
            }),
        ),
    ];
    if let Some(m) = &f.meta {
        generated.push((
            "@version",
            Box::new(move |o: &mut Vec<u8>| {
                let _ = write!(o, "{}", m.version);
            }),
        ));
        generated.push((
            "@timestamp",
            Box::new(move |o: &mut Vec<u8>| string(o, &iso8601(m.timestamp))),
        ));
        generated.push((
            "@changeset",
            Box::new(move |o: &mut Vec<u8>| {
                let _ = write!(o, "{}", m.changeset);
            }),
        ));
        generated.push((
            "@uid",
            Box::new(move |o: &mut Vec<u8>| {
                let _ = write!(o, "{}", m.uid);
            }),
        ));
        generated.push(("@user", Box::new(move |o: &mut Vec<u8>| string(o, &m.user))));
    }
    if let Some(a) = x.area {
        generated.push(("@area", Box::new(move |o: &mut Vec<u8>| measure(o, a))));
    }
    if let Some(l) = x.length {
        generated.push(("@length", Box::new(move |o: &mut Vec<u8>| measure(o, l))));
    }
    if let Some(c) = x.centroid {
        generated.push(("@centroid", Box::new(move |o: &mut Vec<u8>| position(o, &c))));
    }
    if let Some(c) = x.label_point {
        generated.push(("@labelPoint", Box::new(move |o: &mut Vec<u8>| position(o, &c))));
    }
    if let Some(parents) = &x.member_of {
        generated.push((
            "@memberOf",
            Box::new(move |o: &mut Vec<u8>| {
                o.push(b'[');
                for (i, p) in parents.iter().enumerate() {
                    if i > 0 {
                        o.push(b',');
                    }
                    let _ = write!(o, "{{\"id\":{},\"role\":", p.id);
                    string(o, &p.role);
                    o.extend_from_slice(b",\"tags\":{");
                    for (k, (key, value)) in p.tags.iter().enumerate() {
                        if k > 0 {
                            o.push(b',');
                        }
                        string(o, key);
                        o.push(b':');
                        string(o, value);
                    }
                    o.extend_from_slice(b"}}");
                }
                o.push(b']');
            }),
        ));
    }

    out.extend_from_slice(b"{\"type\":\"Feature\",\"id\":\"");
    let _ = write!(out, "{kind}/{}", f.id);
    out.extend_from_slice(b"\",\"properties\":{");
    let mut first = true;
    for (k, v) in &f.tags {
        if k.starts_with('@') && generated.iter().any(|(g, _)| g == k) {
            continue;
        }
        if !first {
            out.push(b',');
        }
        first = false;
        string(out, k);
        out.push(b':');
        string(out, v);
    }
    for (k, w) in &generated {
        if !first {
            out.push(b',');
        }
        first = false;
        string(out, k);
        out.push(b':');
        w(out);
    }
    out.push(b'}');
    if let Some(b) = x.bbox {
        out.extend_from_slice(b",\"bbox\":[");
        for (i, v) in b.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            coord(out, *v);
        }
        out.push(b']');
    }
    out.extend_from_slice(b",\"geometry\":");
    write_geometry(&f.geometry, out);
    out.push(b'}');
}

/// Features as GeoJSON lines.
pub fn write_lines(features: &[Feature], opts: GeoJsonOptions, out: &mut Vec<u8>) {
    for f in features {
        if opts.record_separator {
            out.push(0x1e);
        }
        write_feature(f, out);
        out.push(b'\n');
    }
}

/// Serialised features.
struct Chunk {
    bytes: Vec<u8>,
    features: u64,
}

impl Weight for Chunk {
    fn weight(&self) -> u64 {
        self.features
    }
}

/// Export features straight into newline-delimited GeoJSON, serialised on
/// the worker threads and written in file order. `progress` sees each
/// block's progress and can stop the export.
pub fn write_geojson_seq(
    file: &PbfFile,
    opts: &FeatureOptions,
    geojson: GeoJsonOptions,
    out: &mut dyn Write,
    mut progress: impl FnMut(Progress) -> Flow,
) -> Result<ExportStats, ExportError> {
    let mut err = None;
    let stats = export_features_with(
        file,
        opts,
        move |fs| {
            let mut bytes = Vec::with_capacity(fs.len() * 256);
            write_lines(&fs, geojson, &mut bytes);
            vec![Chunk {
                bytes,
                features: fs.len() as u64,
            }]
        },
        |e| match e {
            Event::Batch(chunks) => {
                for c in chunks {
                    if let Err(e) = out.write_all(&c.bytes) {
                        err = Some(e);
                        return Flow::Stop;
                    }
                }
                Flow::Continue
            }
            Event::Progress(p) => progress(p),
        },
    )?;
    if let Some(e) = err {
        return Err(ExportError::Io(e));
    }
    out.flush().map_err(ExportError::Io)?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrich::{Extras, Parent};
    use crate::record::Meta;
    use tilemaker_pbf::ElementType;

    #[test]
    fn numbers_and_strings() {
        let mut o = Vec::new();
        for v in [0.0, -0.5, 7.4219341, -179.9999999, 12.0, 1e-7, -1e-7] {
            coord(&mut o, v);
            o.push(b' ');
        }
        assert_eq!(
            String::from_utf8(o).unwrap(),
            "0 -0.5 7.4219341 -179.9999999 12 0.0000001 -0.0000001 "
        );
        let mut o = Vec::new();
        string(&mut o, "a\"b\\c\nd\u{1}é");
        assert_eq!(String::from_utf8(o).unwrap(), "\"a\\\"b\\\\c\\nd\\u0001é\"");
        assert_eq!(iso8601(1_600_000_000), "2020-09-13T12:26:40Z");
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(-86_400), "1969-12-31T00:00:00Z");
    }

    /// Numbers as floats, so that `1` equals `1.0`.
    fn floats(v: serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Number(n) => serde_json::json!(n.as_f64().unwrap()),
            serde_json::Value::Array(a) => serde_json::Value::Array(a.into_iter().map(floats).collect()),
            serde_json::Value::Object(o) => {
                serde_json::Value::Object(o.into_iter().map(|(k, v)| (k, floats(v))).collect())
            }
            other => other,
        }
    }

    #[test]
    fn matches_serde_output() {
        let f = Feature {
            kind: ElementType::Relation,
            id: 42,
            tags: vec![("name".into(), "Zée \"x\"".into()), ("@id".into(), "tag".into())],
            geometry: Geometry {
                kind: GeometryType::MultiPolygon,
                coords: vec![
                    0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 0.0, 5.0, 5.0, 6.0, 5.0, 6.0, 6.0, 5.0, 5.0,
                ],
                parts: vec![4, 8],
                polygons: vec![1, 2],
            },
            meta: Some(Meta {
                version: 3,
                timestamp: 1_600_000_000,
                changeset: 9,
                uid: 7,
                user: "ann".into(),
                visible: true,
            }),
            extras: Extras {
                area: Some(1234.56789),
                bbox: Some([0.0, 0.0, 6.0, 6.0]),
                member_of: Some(vec![Parent {
                    id: 7,
                    role: "outer".into(),
                    tags: vec![("type".to_string(), "boundary".to_string())].into(),
                }]),
                ..Default::default()
            },
        };
        let mut o = Vec::new();
        write_feature(&f, &mut o);
        let v: serde_json::Value = serde_json::from_slice(&o).unwrap();
        assert_eq!(v["id"], "relation/42");
        assert_eq!(v["properties"]["@id"], 42);
        assert_eq!(v["properties"]["name"], "Zée \"x\"");
        assert_eq!(v["properties"]["@timestamp"], "2020-09-13T12:26:40Z");
        assert_eq!(v["properties"]["@area"], 1234.568);
        assert_eq!(v["properties"]["@memberOf"][0]["tags"]["type"], "boundary");
        assert_eq!(v["bbox"], serde_json::json!([0, 0, 6, 6]));
        assert_eq!(floats(v["geometry"].clone()), floats(f.geometry.to_geojson()));
        let lines = Geometry {
            kind: GeometryType::MultiLineString,
            coords: vec![0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.5],
            parts: vec![2, 4],
            polygons: vec![],
        };
        let mut o = Vec::new();
        write_geometry(&lines, &mut o);
        let v: serde_json::Value = serde_json::from_slice(&o).unwrap();
        assert_eq!(floats(v), floats(lines.to_geojson()));
    }
}
