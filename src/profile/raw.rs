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

/// A samply pid. The Firefox processed-profile schema permits either a plain
/// integer or a string with a `.N` suffix (e.g. `"1969186.1"`) to distinguish
/// distinct processes that share the same OS pid (e.g. the parent samply
/// recorder vs. forked targets). The base [`u64`] alone is lossy — we keep
/// the suffix so describe_profile can bucket them apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pid {
    pub value: u64,
    pub suffix: Option<u32>,
}

impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.suffix {
            Some(s) => write!(f, "{}.{}", self.value, s),
            None => write!(f, "{}", self.value),
        }
    }
}

impl<'de> Deserialize<'de> for Pid {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        use serde_json::Value;
        let v = Value::deserialize(de)?;
        match v {
            Value::Number(n) => {
                let value = n
                    .as_u64()
                    .or_else(|| n.as_f64().map(|f| f as u64))
                    .ok_or_else(|| D::Error::custom("cannot convert number to u64"))?;
                Ok(Pid {
                    value,
                    suffix: None,
                })
            }
            Value::String(s) => {
                let mut parts = s.splitn(2, '.');
                let value = parts
                    .next()
                    .unwrap_or("")
                    .parse::<u64>()
                    .map_err(D::Error::custom)?;
                let suffix = parts.next().and_then(|p| p.parse::<u32>().ok());
                Ok(Pid { value, suffix })
            }
            other => Err(D::Error::custom(format!(
                "expected number or string, got {other}"
            ))),
        }
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

/// A single DWARF inline-frame record attached to a native frame address.
/// Pollard captures these from `wholesym::AddressInfo.frames[..len-1]`
/// during symbolication, indexed parallel to [`RawFrameTable`] so the
/// query layer can fan out a single profile-level frame into the chain
/// of inlined call sites it represents.
#[derive(Debug, Clone, Default)]
pub struct InlineFrame {
    pub function: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThread {
    #[serde(deserialize_with = "deserialize_id_as_u64")]
    pub tid: u64,
    pub pid: Pid,
    #[serde(default)]
    pub name: Option<String>,
    /// Per-thread process name (e.g. "rustfmt", "Compositor"). Firefox's
    /// processed-profile schema attaches this at the thread level — not on a
    /// separate process record — so describe_profile recovers it by reading
    /// any thread that belongs to the pid.
    #[serde(default)]
    pub process_name: Option<String>,
    pub register_time: f64,
    pub string_array: Vec<String>,
    pub frame_table: RawFrameTable,
    pub func_table: RawFuncTable,
    pub stack_table: RawStackTable,
    pub samples: RawSampleTable,
    pub resource_table: RawResourceTable,
    #[serde(default)]
    pub native_symbols: Option<RawNativeSymbols>,
    /// Per-thread marker stream. samply uses this to land non-cycles
    /// hardware counter samples (cache-misses, branch-misses,
    /// instructions, …); each such marker carries a `data.cause.stack`
    /// pointing into the same stack table the samples track uses.
    /// `default` so older fixtures without a markers field still parse.
    #[serde(default)]
    pub markers: RawMarkerTable,
    /// Per-frame inline-call chain (innermost-first), populated by
    /// [`crate::profile::symbolicate`]. Index parallel to [`RawFrameTable`];
    /// empty Vec when no inline records exist or symbolication wasn't run.
    /// Not part of the Firefox processed-profile schema — pollard-internal,
    /// hence skipped during deserialization.
    #[serde(skip)]
    pub inline_chains: Vec<Vec<InlineFrame>>,
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

/// Per-thread marker stream. The Firefox schema uses this for arbitrary
/// point-or-range events; samply lands secondary perf events
/// (cache-misses, branch-misses, instructions) here when the recorder is
/// asked to produce more than one event per sample.
///
/// Pollard models only what its query layer needs:
///   * `name[i]` → string-array index, resolves to the marker's event
///     name (e.g. `"cache-misses"`).
///   * `data[i]` → optional payload; the resolver pulls `cause.stack`
///     out so the same stack-walking code as the samples track can
///     attribute the event to a function.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RawMarkerTable {
    pub length: usize,
    /// Parallel to `length`. Entries may be `null` for markers without
    /// structured data (e.g. text-only annotations); we keep them as
    /// `None` so the index alignment with `name`/`startTime` survives.
    pub data: Vec<Option<RawMarkerData>>,
    /// String-array indices. Resolve via `thread.string_array[name[i]]`.
    pub name: Vec<usize>,
    pub start_time: Vec<f64>,
    pub end_time: Vec<f64>,
    pub phase: Vec<u8>,
    pub category: Vec<usize>,
}

/// Marker payload subset. Only `cause.stack` is consumed; other fields
/// (`type`, text, timestamps embedded in the payload) are ignored.
/// Defaulting `cause` to `None` keeps us forward-compatible with text or
/// log-style markers that the Firefox schema also permits.
#[derive(Debug, Deserialize, Default)]
pub struct RawMarkerData {
    #[serde(default)]
    pub cause: Option<MarkerCause>,
}

#[derive(Debug, Deserialize, Default)]
pub struct MarkerCause {
    /// Stack-table index. Same shape as `samples.stack[i]`.
    pub stack: usize,
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
    fn deserializes_marker_table() {
        let with_markers = MINIMAL.replace(
            r#""markers": {"length": 0, "data": [], "name": [], "startTime": [], "endTime": [], "phase": [], "category": []}"#,
            r#""markers": {
                "length": 2,
                "data": [{"type": "Other event", "cause": {"stack": 7}}, null],
                "name": [1, 1],
                "startTime": [0.0, 1.0],
                "endTime": [0.0, 1.0],
                "phase": [0, 0],
                "category": [0, 0]
            }"#,
        );
        let p: RawProfile = serde_json::from_str(&with_markers).unwrap();
        let m = &p.threads[0].markers;
        assert_eq!(m.length, 2);
        assert_eq!(m.name, vec![1, 1]);
        assert_eq!(
            m.data[0].as_ref().and_then(|d| d.cause.as_ref()).map(|c| c.stack),
            Some(7)
        );
        assert!(m.data[1].is_none());
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
