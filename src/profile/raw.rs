//! Raw serde-derived structs that match the Firefox processed-profile JSON.
//! Field naming uses serde-rename to convert from camelCase JSON.
//!
//! These types intentionally do NOT cover every field in the schema — only
//! what v1 query tools touch. Unknown fields are ignored.

#![allow(dead_code)]

use serde::{Deserialize, Deserializer};

/// Deserialize a pid/tid that may be encoded as a JSON integer, float, or
/// quoted string (e.g. `"50258"` or `"50258.1"` in real samply output).
/// We extract the integer part and discard fractional suffixes like `.1`.
fn deserialize_id_as_u64<'de, D: Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    use serde::de::Error;
    use serde_json::Value;
    let v = Value::deserialize(de)?;
    match v {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(u)
            } else if let Some(f) = n.as_f64() {
                Ok(f as u64)
            } else {
                Err(D::Error::custom("cannot convert number to u64"))
            }
        }
        Value::String(s) => {
            // Take the part before any `.` (handles "50258.1")
            let base = s.split('.').next().unwrap_or(&s);
            base.parse::<u64>().map_err(D::Error::custom)
        }
        other => Err(D::Error::custom(format!(
            "expected number or string, got {other}"
        ))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawProfile {
    pub meta: RawMeta,
    #[serde(default)]
    pub libs: Vec<RawLib>,
    #[serde(default)]
    pub threads: Vec<RawThread>,
    #[serde(default)]
    pub processes: Vec<RawProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawMeta {
    pub interval: f64,
    pub start_time: f64,
    #[serde(default)]
    pub product: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawLib {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub debug_name: Option<String>,
    #[serde(default)]
    pub debug_path: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub breakpad_id: Option<String>,
    #[serde(default)]
    pub code_id: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThread {
    #[serde(deserialize_with = "deserialize_id_as_u64")]
    pub tid: u64,
    #[serde(deserialize_with = "deserialize_id_as_u64")]
    pub pid: u64,
    #[serde(default)]
    pub name: Option<String>,
    pub register_time: f64,
    pub string_array: Vec<String>,
    pub frame_table: RawFrameTable,
    pub func_table: RawFuncTable,
    pub stack_table: RawStackTable,
    pub samples: RawSampleTable,
    pub resource_table: RawResourceTable,
    #[serde(default)]
    pub native_symbols: Option<RawNativeSymbols>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFrameTable {
    pub length: usize,
    pub address: Vec<i64>, // -1 for non-native
    pub func: Vec<usize>,
    pub line: Vec<Option<u32>>,
    pub column: Vec<Option<u32>>,
    pub category: Vec<Option<usize>>,
    pub subcategory: Vec<Option<usize>>,
    #[serde(default)]
    pub native_symbol: Vec<Option<usize>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFuncTable {
    pub length: usize,
    pub name: Vec<usize>, // string-array index
    #[serde(rename = "isJS")]
    pub is_js: Vec<bool>,
    #[serde(rename = "relevantForJS")]
    pub relevant_for_js: Vec<bool>,
    pub resource: Vec<i32>,            // -1 if no resource
    pub file_name: Vec<Option<usize>>, // string-array index
    pub line_number: Vec<Option<u32>>,
    pub column_number: Vec<Option<u32>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStackTable {
    pub length: usize,
    pub frame: Vec<usize>,
    #[serde(default)]
    pub category: Vec<usize>,
    #[serde(default)]
    pub subcategory: Vec<usize>,
    pub prefix: Vec<Option<usize>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSampleTable {
    pub length: usize,
    pub stack: Vec<Option<usize>>,
    /// Absolute timestamps (ms). Present in synthetic / pre-processed profiles.
    #[serde(default)]
    pub time: Vec<f64>,
    /// Relative time deltas (ms). Present in raw samply output.
    /// Use [`Self::absolute_times`] to get absolute timestamps from either
    /// field.
    #[serde(default)]
    pub time_deltas: Vec<f64>,
    #[serde(default)]
    pub weight: Option<Vec<f64>>,
    #[serde(default)]
    pub weight_type: Option<String>,
}

impl RawSampleTable {
    /// Return absolute timestamps regardless of whether the profile stores
    /// `time` or `timeDeltas`.
    pub fn absolute_times(&self) -> Vec<f64> {
        if !self.time.is_empty() {
            return self.time.clone();
        }
        // Convert time deltas to absolute by summing.
        let mut acc = 0.0f64;
        self.time_deltas
            .iter()
            .map(|&d| {
                acc += d;
                acc
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawResourceTable {
    pub length: usize,
    pub lib: Vec<Option<usize>>,
    pub name: Vec<usize>, // string-array index
    pub host: Vec<Option<usize>>,
    #[serde(rename = "type")]
    pub type_: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawNativeSymbols {
    pub length: usize,
    pub lib_index: Vec<usize>,
    pub address: Vec<i64>,
    pub name: Vec<usize>, // string-array index
    pub function_size: Vec<Option<u64>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
        "libs": [],
        "threads": [{
            "name": "Main",
            "tid": 1,
            "pid": 1,
            "registerTime": 0.0,
            "stringArray": ["foo", "bar"],
            "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "innerWindowID": [], "implementation": [], "line": [], "column": [], "nativeSymbol": []},
            "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
            "samples": {"length": 0, "stack": [], "time": [], "weight": null, "weightType": "samples"},
            "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
            "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
            "markers": {"length": 0, "data": [], "name": [], "startTime": [], "endTime": [], "phase": [], "category": []},
            "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []},
            "processType": "default",
            "processStartupTime": 0.0
        }]
    }"#;

    #[test]
    fn deserializes_minimal_profile() {
        let p: RawProfile = serde_json::from_str(MINIMAL).unwrap();
        assert_eq!(p.threads.len(), 1);
        assert_eq!(p.threads[0].name.as_deref(), Some("Main"));
    }

    #[test]
    fn extra_fields_are_ignored() {
        let with_extras = MINIMAL.replace(
            r#""product": "test"#,
            r#""unknownField": 42, "product": "test"#,
        );
        serde_json::from_str::<RawProfile>(&with_extras).unwrap();
    }
}
