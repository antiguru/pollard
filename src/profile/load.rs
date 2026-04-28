//! Read a profile file (.json or .json.gz) into our raw types.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::profile::raw::RawProfile;
use flate2::bufread::GzDecoder;
use std::ffi::OsStr;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

pub fn load_from_path(path: &Path) -> Result<RawProfile, ToolError> {
    let file = File::open(path).map_err(|_| ToolError::FileNotFound {
        path: path.to_path_buf(),
    })?;
    let buf_reader = BufReader::new(file);

    if path.extension() == Some(OsStr::new("gz")) {
        let decoder = GzDecoder::new(buf_reader);
        let gz_reader = BufReader::new(decoder);
        serde_json::from_reader(gz_reader).map_err(|e| ToolError::NotAProfile {
            path: path.to_path_buf(),
            details: e.to_string(),
        })
    } else {
        serde_json::from_reader(buf_reader).map_err(|e| ToolError::NotAProfile {
            path: path.to_path_buf(),
            details: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    const MINIMAL: &str = include_str!("../../tests/fixtures/minimal_profile.json");

    #[test]
    fn loads_uncompressed_json() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        f.write_all(MINIMAL.as_bytes()).unwrap();
        let p = load_from_path(f.path()).unwrap();
        assert!(!p.threads.is_empty());
    }

    #[test]
    fn loads_gzipped_json() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut f = NamedTempFile::with_suffix(".json.gz").unwrap();
        let mut gz = GzEncoder::new(f.as_file_mut(), Compression::default());
        gz.write_all(MINIMAL.as_bytes()).unwrap();
        gz.finish().unwrap();
        let p = load_from_path(f.path()).unwrap();
        assert!(!p.threads.is_empty());
    }

    #[test]
    fn missing_file_returns_file_not_found() {
        let err = load_from_path(std::path::Path::new("/no/such/file.json")).unwrap_err();
        assert!(matches!(err, crate::error::ToolError::FileNotFound { .. }));
    }
}
