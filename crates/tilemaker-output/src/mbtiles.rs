//! MBTiles (SQLite) output, modelled on tilemaker's `mbtiles.cpp`.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::{compress, decompress, extra_value_string, Compression, OutputError, TileSink, TilesetMetadata};

/// Number of tile inserts per transaction.
const BATCH_SIZE: usize = 10_000;

/// Writes tiles into an `.mbtiles` SQLite database.
pub struct MbtilesWriter {
    conn: Connection,
    path: PathBuf,
    compression: Compression,
    merge: bool,
    in_transaction: bool,
    pending: usize,
}

impl std::fmt::Debug for MbtilesWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MbtilesWriter")
            .field("path", &self.path)
            .field("compression", &self.compression)
            .field("merge", &self.merge)
            .finish()
    }
}

impl MbtilesWriter {
    /// Creates (or with `merge`, opens) an MBTiles file. Without `merge` any
    /// existing file is deleted first.
    pub fn create(path: impl AsRef<Path>, compression: Compression, merge: bool) -> Result<Self, OutputError> {
        let path = path.as_ref().to_path_buf();
        if !merge && path.exists() {
            std::fs::remove_file(&path)?;
        }
        let is_new = !path.exists();
        let conn = Connection::open(&path)?;

        conn.execute_batch("PRAGMA synchronous = OFF;")?;
        // Non-fatal pragmas, as in tilemaker.
        let _ = conn.execute_batch("PRAGMA application_id = 0x4d504258;");
        let _ = conn.query_row("PRAGMA journal_mode = MEMORY;", [], |_| Ok(()));
        if is_new {
            conn.execute_batch("PRAGMA encoding = 'UTF-8'; PRAGMA page_size = 65536; VACUUM;")?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS metadata (name text, value text, UNIQUE (name));
             CREATE TABLE IF NOT EXISTS tiles (zoom_level integer, tile_column integer, tile_row integer, tile_data blob);
             CREATE UNIQUE INDEX IF NOT EXISTS tile_index on tiles (zoom_level, tile_column, tile_row);",
        )?;
        conn.execute_batch("BEGIN;")?;

        Ok(MbtilesWriter {
            conn,
            path,
            compression,
            merge,
            in_transaction: true,
            pending: 0,
        })
    }

    /// Writes (replaces) one metadata row.
    pub fn write_metadata(&mut self, name: &str, value: &str) -> Result<(), OutputError> {
        self.conn
            .prepare_cached("INSERT OR REPLACE INTO metadata (name, value) VALUES (?1, ?2)")?
            .execute(params![name, value])?;
        Ok(())
    }

    /// Reads a metadata value.
    pub fn read_metadata(&self, name: &str) -> Result<Option<String>, OutputError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM metadata WHERE name = ?1", [name], |r| r.get(0))
            .optional()?)
    }

    /// Reads a tile (XYZ coordinates) and returns its decompressed bytes.
    pub fn read_tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, OutputError> {
        let data: Option<Vec<u8>> = self
            .conn
            .prepare_cached("SELECT tile_data FROM tiles WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3")?
            .query_row(params![z, x, tms_row(z, y)], |r| r.get(0))
            .optional()?;
        match data {
            Some(d) => Ok(Some(decompress(&d)?)),
            None => Ok(None),
        }
    }

    fn commit(&mut self) -> Result<(), OutputError> {
        if self.in_transaction {
            self.conn.execute_batch("COMMIT;")?;
            self.in_transaction = false;
        }
        Ok(())
    }

    /// Existing `bounds` metadata parsed as `[minlon, minlat, maxlon, maxlat]`.
    fn existing_bounds(&self) -> Result<Option<[f64; 4]>, OutputError> {
        let Some(s) = self.read_metadata("bounds")? else {
            return Ok(None);
        };
        let v: Vec<f64> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        Ok((v.len() == 4).then(|| [v[0], v[1], v[2], v[3]]))
    }
}

/// Converts an XYZ row to the TMS row used by MBTiles.
fn tms_row(z: u8, y: u32) -> u32 {
    ((1u64 << z) - 1 - u64::from(y)) as u32
}

impl TileSink for MbtilesWriter {
    fn write_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<(), OutputError> {
        let blob = compress(data, self.compression);
        if !self.in_transaction {
            self.conn.execute_batch("BEGIN;")?;
            self.in_transaction = true;
        }
        self.conn
            .prepare_cached(
                "INSERT OR REPLACE INTO tiles (zoom_level, tile_column, tile_row, tile_data) VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![z, x, tms_row(z, y), blob])?;
        self.pending += 1;
        if self.pending >= BATCH_SIZE {
            self.pending = 0;
            self.conn.execute_batch("COMMIT; BEGIN;")?;
        }
        Ok(())
    }

    fn finish(&mut self, meta: &TilesetMetadata) -> Result<(), OutputError> {
        if !self.in_transaction {
            self.conn.execute_batch("BEGIN;")?;
            self.in_transaction = true;
        }
        let mut bounds = meta.bounds;
        if self.merge {
            if let Some(old) = self.existing_bounds()? {
                bounds = Some(match bounds {
                    Some(b) => [b[0].min(old[0]), b[1].min(old[1]), b[2].max(old[2]), b[3].max(old[3])],
                    None => old,
                });
            }
        }

        self.write_metadata("name", &meta.name)?;
        self.write_metadata("type", "baselayer")?;
        self.write_metadata("version", &meta.version)?;
        self.write_metadata("description", &meta.description)?;
        self.write_metadata("format", "pbf")?;
        self.write_metadata("minzoom", &meta.minzoom.to_string())?;
        self.write_metadata("maxzoom", &meta.maxzoom.to_string())?;
        if let Some(b) = bounds {
            self.write_metadata("bounds", &format!("{:.6},{:.6},{:.6},{:.6}", b[0], b[1], b[2], b[3]))?;
        }
        let center_meta = TilesetMetadata { bounds, ..meta.clone() };
        if let Some((lon, lat, z)) = center_meta.effective_center() {
            self.write_metadata("center", &format!("{lon:.6},{lat:.6},{z}"))?;
        }
        let layers = if meta.vector_layers_json.trim().is_empty() {
            "[]"
        } else {
            meta.vector_layers_json.as_str()
        };
        self.write_metadata("json", &format!("{{\"vector_layers\":{layers}}}"))?;
        for (k, v) in &meta.extra {
            self.write_metadata(k, &extra_value_string(v))?;
        }
        self.commit()
    }

    fn read_tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, OutputError> {
        MbtilesWriter::read_tile(self, z, x, y)
    }
}

impl Drop for MbtilesWriter {
    fn drop(&mut self) {
        let _ = self.commit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::temp_path;

    fn meta() -> TilesetMetadata {
        TilesetMetadata {
            name: "test".into(),
            version: "1.0".into(),
            description: "desc".into(),
            minzoom: 0,
            maxzoom: 14,
            bounds: Some([-1.0, 50.0, 1.0, 52.0]),
            center: None,
            vector_layers_json: r#"[{"id":"water","fields":{},"minzoom":0,"maxzoom":14}]"#.into(),
            extra: vec![
                ("attribution".into(), serde_json::json!("© OSM")),
                ("custom".into(), serde_json::json!({"a": 1})),
            ],
        }
    }

    #[test]
    fn roundtrip_and_metadata() {
        let path = temp_path("rt.mbtiles");
        {
            let mut w = MbtilesWriter::create(&path, Compression::Gzip, false).unwrap();
            for i in 0..25_000u32 {
                let z = 14;
                w.write_tile(z, i % 16384, i / 16384, format!("tile{i}").as_bytes())
                    .unwrap();
            }
            w.write_tile(3, 1, 2, b"z3").unwrap();
            w.write_tile(3, 1, 2, b"z3 replaced").unwrap();
            w.finish(&meta()).unwrap();
        }

        let conn = Connection::open(&path).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tiles", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 25_001);
        let blob: Vec<u8> = conn
            .query_row(
                "SELECT tile_data FROM tiles WHERE zoom_level=3 AND tile_column=1 AND tile_row=5",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(&blob[..2], &[0x1f, 0x8b]);
        assert_eq!(decompress(&blob).unwrap(), b"z3 replaced");

        let get = |k: &str| -> String {
            conn.query_row("SELECT value FROM metadata WHERE name=?1", [k], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(get("name"), "test");
        assert_eq!(get("type"), "baselayer");
        assert_eq!(get("format"), "pbf");
        assert_eq!(get("minzoom"), "0");
        assert_eq!(get("maxzoom"), "14");
        assert_eq!(get("bounds"), "-1.000000,50.000000,1.000000,52.000000");
        assert_eq!(get("center"), "0.000000,51.000000,7");
        assert_eq!(get("attribution"), "© OSM");
        assert_eq!(get("custom"), r#"{"a":1}"#);
        let json: serde_json::Value = serde_json::from_str(&get("json")).unwrap();
        assert_eq!(json["vector_layers"][0]["id"], "water");
        drop(conn);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn merge_reads_existing() {
        let path = temp_path("merge.mbtiles");
        {
            let mut w = MbtilesWriter::create(&path, Compression::Deflate, false).unwrap();
            w.write_tile(5, 3, 7, b"old").unwrap();
            w.finish(&meta()).unwrap();
        }
        {
            let mut w = MbtilesWriter::create(&path, Compression::Deflate, true).unwrap();
            assert_eq!(w.read_tile(5, 3, 7).unwrap().as_deref(), Some(&b"old"[..]));
            assert_eq!(w.read_tile(5, 3, 8).unwrap(), None);
            w.write_tile(5, 3, 7, b"new").unwrap();
            assert_eq!(w.read_tile(5, 3, 7).unwrap().as_deref(), Some(&b"new"[..]));
            let mut m = meta();
            m.bounds = Some([0.0, 49.0, 2.0, 51.0]);
            w.finish(&m).unwrap();
            assert_eq!(
                w.read_metadata("bounds").unwrap().unwrap(),
                "-1.000000,49.000000,2.000000,52.000000"
            );
        }
        {
            // Non-merge open truncates.
            let w = MbtilesWriter::create(&path, Compression::Deflate, false).unwrap();
            assert_eq!(w.read_tile(5, 3, 7).unwrap(), None);
        }
        std::fs::remove_file(&path).unwrap();
    }
}
