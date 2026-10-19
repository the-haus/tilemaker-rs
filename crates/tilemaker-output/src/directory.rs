//! Directory output: `{dir}/{z}/{x}/{y}.pbf` plus `metadata.json`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{compress, decompress, Compression, OutputError, TileSink, TilesetMetadata};

/// Writes each tile to its own file.
#[derive(Debug)]
pub struct DirectoryWriter {
    root: PathBuf,
    compression: Compression,
    /// `(z, x)` directories already created.
    created: HashSet<(u8, u32)>,
}

impl DirectoryWriter {
    /// Creates the output directory (if needed).
    pub fn create(dir: impl AsRef<Path>, compression: Compression) -> Result<Self, OutputError> {
        let root = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        Ok(DirectoryWriter {
            root,
            compression,
            created: HashSet::new(),
        })
    }

    fn tile_path(&self, z: u8, x: u32, y: u32) -> PathBuf {
        self.root
            .join(z.to_string())
            .join(x.to_string())
            .join(format!("{y}.pbf"))
    }
}

impl TileSink for DirectoryWriter {
    fn write_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<(), OutputError> {
        if self.created.insert((z, x)) {
            std::fs::create_dir_all(self.root.join(z.to_string()).join(x.to_string()))?;
        }
        std::fs::write(self.tile_path(z, x, y), compress(data, self.compression))?;
        Ok(())
    }

    /// Writes `metadata.json` in tilemaker's format: the `filemetadata`
    /// entries, then bounds, name, version, description, zooms and
    /// vector_layers.
    fn finish(&mut self, meta: &TilesetMetadata) -> Result<(), OutputError> {
        let mut doc = Map::new();
        for (k, v) in &meta.extra {
            doc.insert(k.clone(), v.clone());
        }
        if let Some(b) = meta.bounds {
            doc.insert("bounds".into(), json!(b));
        }
        doc.insert("name".into(), json!(meta.name));
        doc.insert("version".into(), json!(meta.version));
        doc.insert("description".into(), json!(meta.description));
        doc.insert("minzoom".into(), json!(meta.minzoom));
        doc.insert("maxzoom".into(), json!(meta.maxzoom));
        doc.insert("vector_layers".into(), meta.vector_layers());
        let text = serde_json::to_string(&Value::Object(doc)).map_err(|e| OutputError::Other(e.to_string()))?;
        std::fs::write(self.root.join("metadata.json"), text)?;
        Ok(())
    }

    fn read_tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, OutputError> {
        match std::fs::read(self.tile_path(z, x, y)) {
            Ok(d) => Ok(Some(decompress(&d)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::temp_path;

    #[test]
    fn writes_tiles_and_metadata() {
        let dir = temp_path("dir");
        let mut w = DirectoryWriter::create(&dir, Compression::Gzip).unwrap();
        w.write_tile(2, 1, 3, b"hello").unwrap();
        w.write_tile(2, 1, 0, b"world").unwrap();
        let meta = TilesetMetadata {
            name: "n".into(),
            minzoom: 0,
            maxzoom: 2,
            bounds: Some([1.0, 2.0, 3.0, 4.0]),
            vector_layers_json: r#"[{"id":"a"}]"#.into(),
            extra: vec![("attribution".into(), json!("x"))],
            ..Default::default()
        };
        w.finish(&meta).unwrap();

        let raw = std::fs::read(dir.join("2/1/3.pbf")).unwrap();
        assert_eq!(&raw[..2], &[0x1f, 0x8b]);
        assert_eq!(w.read_tile(2, 1, 3).unwrap().unwrap(), b"hello");
        assert_eq!(w.read_tile(2, 1, 1).unwrap(), None);
        let md: Value = serde_json::from_slice(&std::fs::read(dir.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(md["bounds"], json!([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(md["vector_layers"][0]["id"], "a");
        assert_eq!(md["attribution"], "x");
        assert_eq!(md["maxzoom"], 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
