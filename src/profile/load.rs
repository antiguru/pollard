//! Read a profile file into our raw types.
//!
//! The container is detected from magic bytes: gzip first, then JSLB,
//! and plain JSON otherwise. Only the JSLB path builds a
//! `serde_json::Value`, because its skeleton is small and the columns
//! live in binary slabs.

#![allow(dead_code)]

use std::io::Read;
use std::path::Path;

use crate::error::ToolError;
use crate::profile::raw::RawProfile;
use crate::profile::wire::is_supported_version;

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Processed-profile versions the decoders understand, for error
/// messages. Keep in sync with [`is_supported_version`].
pub const SUPPORTED_VERSIONS: &str = "49-55, 75";

#[derive(Debug)]
pub(crate) enum LoadError {
    Gzip(String),
    Jslb(String),
    Json(String),
    UnsupportedVersion(u32),
}

impl LoadError {
    fn into_tool_error(self, path: &Path) -> ToolError {
        let path = path.to_path_buf();
        match self {
            LoadError::UnsupportedVersion(v) => ToolError::UnsupportedProfileFormat {
                path,
                version: v.to_string(),
                supported: SUPPORTED_VERSIONS.to_owned(),
            },
            LoadError::Gzip(d) => ToolError::NotAProfile {
                path,
                details: format!("gzip: {d}"),
            },
            LoadError::Jslb(d) => ToolError::NotAProfile {
                path,
                details: format!("JSLB container: {d}"),
            },
            LoadError::Json(d) => ToolError::NotAProfile { path, details: d },
        }
    }
}

pub fn load_from_path(path: &Path) -> Result<RawProfile, ToolError> {
    let bytes = std::fs::read(path).map_err(|_| ToolError::FileNotFound {
        path: path.to_path_buf(),
    })?;
    // Own the compressed bytes here, rather than routing through
    // `decode_bytes`, so the compressed `Vec` can be dropped before the
    // (usually much larger) decompressed buffer is parsed, instead of
    // both staying resident for the whole parse.
    let result = if bytes.starts_with(&GZIP_MAGIC) {
        let out = decompress_gzip(&bytes).map_err(|e| e.into_tool_error(path))?;
        drop(bytes);
        decode_uncompressed(&out)
    } else {
        decode_uncompressed(&bytes)
    };
    result.map_err(|e| e.into_tool_error(path))
}

fn decompress_gzip(bytes: &[u8]) -> Result<Vec<u8>, LoadError> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .map_err(|e| LoadError::Gzip(e.to_string()))?;
    Ok(out)
}

pub(crate) fn decode_bytes(bytes: &[u8]) -> Result<RawProfile, LoadError> {
    if bytes.starts_with(&GZIP_MAGIC) {
        let out = decompress_gzip(bytes)?;
        return decode_uncompressed(&out);
    }
    decode_uncompressed(bytes)
}

fn decode_uncompressed(bytes: &[u8]) -> Result<RawProfile, LoadError> {
    if bytes.starts_with(&crate::profile::jslb::MAGIC) {
        let value = crate::profile::jslb::to_value(bytes).map_err(LoadError::Jslb)?;
        let version = value
            .pointer("/meta/preprocessedProfileVersion")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        return serde_json::from_value(value).map_err(|e| json_error(e, version));
    }
    serde_json::from_slice(bytes).map_err(|e| {
        // Only the error path pays for a second parse. A file in an
        // unsupported version usually fails schema checks before the
        // version gate runs, so the user needs the version, not the
        // schema message.
        let version = serde_json::from_slice::<VersionPeek>(bytes)
            .ok()
            .and_then(|p| p.meta.preprocessed_profile_version);
        json_error(e, version)
    })
}

fn json_error(e: serde_json::Error, version: Option<u32>) -> LoadError {
    match version {
        Some(v) if !is_supported_version(v) => LoadError::UnsupportedVersion(v),
        _ => LoadError::Json(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct VersionPeek {
    meta: VersionPeekMeta,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionPeekMeta {
    #[serde(default)]
    preprocessed_profile_version: Option<u32>,
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

    #[test]
    fn version_60_reports_unsupported() {
        // Version 60 has `shared` but pre-flag frame columns, so serde
        // fails before the version gate. The error path peeks the version.
        let json = r#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 60},
                       "shared": {"stringArray": [], "frameTable": {"length": 0, "address": []}}, "threads": []}"#;
        let err = decode_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, LoadError::UnsupportedVersion(60)), "{err:?}");
    }

    #[test]
    fn version_76_reports_unsupported() {
        let json = r#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 76}, "threads": []}"#;
        let err = decode_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, LoadError::UnsupportedVersion(76)), "{err:?}");
    }

    #[test]
    fn supported_versions_pass_the_gate() {
        for v in [49, 55] {
            let json = format!(
                r#"{{"meta": {{"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": {v}}}, "threads": []}}"#
            );
            decode_bytes(json.as_bytes()).unwrap();
        }
    }

    #[test]
    fn schema_error_in_supported_version_is_not_a_profile() {
        let json = r#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 55},
                       "threads": [{"tid": 1}]}"#;
        let err = decode_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, LoadError::Json(_)), "{err:?}");
    }

    #[test]
    fn gzipped_jslb_loads_by_magic() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut b = json_slabs::Builder::new();
        let stacks = b.add_slab_from_vec(vec![0i32]);
        let root = format!(
            r#"{{"meta": {{"interval": 1.0, "startTime": 0.0}}, "threads": [{{"tid": 1, "pid": 1, "registerTime": 0.0,
                "stringArray": ["f"],
                "frameTable": {{"length": 1, "address": [-1], "func": [0], "line": [null], "column": [null], "category": [0], "subcategory": [0]}},
                "funcTable": {{"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [-1], "fileName": [null], "lineNumber": [null], "columnNumber": [null]}},
                "stackTable": {{"length": 1, "frame": [0], "prefix": [null]}},
                "resourceTable": {{"length": 0, "lib": [], "name": [], "host": [], "type": []}},
                "samples": {{"length": 1, "stack": {stacks:#}, "time": [0.0]}}}}]}}"#
        );
        let jslb = b.finish(root.as_bytes());
        // A `.json` name must not matter: detection reads magic bytes.
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        let mut gz = GzEncoder::new(f.as_file_mut(), Compression::default());
        gz.write_all(&jslb).unwrap();
        gz.finish().unwrap();
        let p = load_from_path(f.path()).unwrap();
        assert_eq!(p.threads[0].samples.stack, vec![Some(0)]);
    }

    #[test]
    fn jslb_unsupported_version_reports_unsupported() {
        // Same shape as `version_60_reports_unsupported`, but through the
        // JSLB path: the skeleton has no slab placeholders, so no slabs
        // are needed.
        let b = json_slabs::Builder::new();
        let root = br#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 60},
                       "shared": {"stringArray": [], "frameTable": {"length": 0, "address": []}}, "threads": []}"#;
        let jslb = b.finish(root);
        let err = decode_bytes(&jslb).unwrap_err();
        assert!(matches!(err, LoadError::UnsupportedVersion(60)), "{err:?}");
    }

    #[test]
    fn unsupported_version_maps_to_tool_error() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        f.write_all(br#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 74}, "threads": []}"#)
            .unwrap();
        let err = load_from_path(f.path()).unwrap_err();
        match err {
            ToolError::UnsupportedProfileFormat {
                version, supported, ..
            } => {
                assert_eq!(version, "74");
                assert_eq!(supported, SUPPORTED_VERSIONS);
            }
            other => panic!("unexpected error {other:?}"),
        }
    }
}
