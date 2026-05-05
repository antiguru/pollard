//! Structured error envelope returned by every MCP tool.
//!
//! Wherever the LLM has enough information to retry, prefer warning + recovery
//! over a hard failure. See spec §"Error handling" for the full table.

#![allow(dead_code)]

use crate::serde_util::is_zero_usize;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::PathBuf;

/// Cap on entries returned in `available_*` fields of error variants
/// (threads, processes, modules, nearest function matches). Intended
/// to keep error payloads parseable by the LLM — a 1107-thread profile
/// otherwise produced ~32 KB of `available_threads` that the harness
/// truncated mid-string. Excess entries are summarised with an
/// `omitted_count`.
pub const ERROR_LIST_LIMIT: usize = 20;

/// Cap on the per-suggestion length in `nearest_matches`. Mangled-ish
/// Rust names with deep template parameters can run into the thousands
/// of characters; truncating each entry keeps the suggestion list
/// readable when one or two oversized entries would otherwise blow the
/// payload budget.
pub const NEAREST_MATCH_MAX_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum ToolError {
    FileNotFound {
        path: PathBuf,
    },
    NotAProfile {
        path: PathBuf,
        details: String,
    },
    UnsupportedProfileFormat {
        path: PathBuf,
        version: String,
    },
    FunctionNotFound {
        function: String,
        /// Top-`ERROR_LIST_LIMIT` ranked suggestions. Each entry is
        /// length-capped at `NEAREST_MATCH_MAX_CHARS` (suffix `…`) so a
        /// single oversized template doesn't blow the response budget.
        nearest_matches: Vec<String>,
    },
    FunctionAmbiguous {
        function: String,
        candidates: Vec<FunctionCandidate>,
    },
    ThreadNotFound {
        thread: String,
        /// Top-`ERROR_LIST_LIMIT` threads ranked by sample count.
        /// `omitted_thread_count` reports the rest so callers know
        /// they're looking at a slice.
        available_threads: Vec<ThreadRef>,
        #[serde(skip_serializing_if = "is_zero_usize")]
        omitted_thread_count: usize,
    },
    ProcessNotFound {
        process: String,
        /// Top-`ERROR_LIST_LIMIT` processes ranked by sample count.
        /// `omitted_process_count` reports the rest.
        available_processes: Vec<ProcessRef>,
        #[serde(skip_serializing_if = "is_zero_usize")]
        omitted_process_count: usize,
    },
    /// Module filter doesn't match any library. Distinct from
    /// `function_not_found`: a typo in `module=` previously fell through
    /// the function search and surfaced as `function_not_found`, which
    /// confused callers who'd correctly typed the function name.
    ModuleNotFound {
        module: String,
        /// Up to `ERROR_LIST_LIMIT` library names known to the profile.
        available_modules: Vec<String>,
        #[serde(skip_serializing_if = "is_zero_usize")]
        omitted_module_count: usize,
    },
    OutOfBounds {
        original_range: [f64; 2],
        clamped_range: [f64; 2],
    },
    ProfileNotFound {
        profile_id: String,
    },
    ProfileEvicted {
        profile_id: String,
        original_path: PathBuf,
    },
    /// User passed a value for a string-enum-style argument that wasn't
    /// one of the accepted options (string enum, malformed regex,
    /// unknown event name, …). Echo the field, the offending value,
    /// and the full accepted set so the caller can correct in one
    /// retry — same shape as `function_not_found.nearest_matches`.
    InvalidValue {
        field: String,
        value: String,
        accepted: Vec<String>,
        /// Free-form additional context — e.g. the regex parser's caret
        /// diagnostic, or `"omit `event` for the default samples track"`.
        /// Optional so the common case (just `accepted` is enough) stays
        /// quiet.
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
    },
    Internal {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionCandidate {
    pub function: String,
    pub module: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadRef {
    pub tid: u64,
    pub name: String,
    /// Sample count on the thread. Used to rank entries in
    /// `available_threads` so the busiest threads stay visible after
    /// the [`ERROR_LIST_LIMIT`] truncation.
    pub samples: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct ProcessRef {
    /// String form to preserve samply's `.N` sub-process suffix
    /// (e.g. `"100.1"`) — same convention as `ProcessDescription::pid`.
    pub pid: String,
    pub name: String,
    /// Total samples across all threads of the process. Used to rank
    /// entries in `available_processes`.
    #[serde(default)]
    pub samples: u64,
}

/// Truncate `s` to `max_chars` characters, appending an ellipsis when
/// the input is longer. Counts char boundaries, not bytes, so multi-byte
/// names (CJK, accented identifiers) don't slice mid-codepoint.
pub fn truncate_for_error(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Apply [`truncate_for_error`] with [`NEAREST_MATCH_MAX_CHARS`] to each
/// entry, also capping the list itself at [`ERROR_LIST_LIMIT`].
pub fn truncate_nearest_matches(matches: Vec<String>) -> Vec<String> {
    matches
        .into_iter()
        .take(ERROR_LIST_LIMIT)
        .map(|s| truncate_for_error(&s, NEAREST_MATCH_MAX_CHARS))
        .collect()
}

/// Build a [`ToolError::ModuleNotFound`] payload from the full list of
/// known module names. Caps `available_modules` at [`ERROR_LIST_LIMIT`]
/// and reports the rest in `omitted_module_count`.
pub fn module_not_found(module: &str, mut all_modules: Vec<String>) -> ToolError {
    let total = all_modules.len();
    all_modules.truncate(ERROR_LIST_LIMIT);
    let omitted_module_count = total.saturating_sub(all_modules.len());
    ToolError::ModuleNotFound {
        module: module.to_owned(),
        available_modules: all_modules,
        omitted_module_count,
    }
}

pub type ToolResult<T> = Result<T, ToolError>;

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string(self).unwrap_or_else(|_| "<error serialization failed>".into())
        )
    }
}

impl std::error::Error for ToolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_not_found_serializes_with_nearest_matches() {
        let err = ToolError::FunctionNotFound {
            function: "memcyp".into(),
            nearest_matches: vec!["memcpy".into(), "mempcpy".into()],
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error"], "function_not_found");
        assert_eq!(json["function"], "memcyp");
        assert_eq!(json["nearest_matches"][0], "memcpy");
    }

    #[test]
    fn out_of_bounds_carries_clamp_info() {
        let err = ToolError::OutOfBounds {
            original_range: [0.0, 99999.0],
            clamped_range: [0.0, 30000.0],
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error"], "out_of_bounds");
        assert_eq!(json["clamped_range"][1], 30000.0);
    }

    #[test]
    fn truncate_for_error_appends_ellipsis_when_too_long() {
        // 250-char identifier → keep 200 chars + ellipsis.
        let long = "a".repeat(250);
        let truncated = truncate_for_error(&long, 200);
        assert_eq!(truncated.chars().count(), 201);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_for_error_passes_short_strings_through() {
        let short = "memcpy";
        assert_eq!(truncate_for_error(short, 200), "memcpy");
    }

    #[test]
    fn truncate_for_error_respects_char_boundaries() {
        // Multi-byte chars: `é` is 2 bytes, but counts as one char.
        let s: String = std::iter::repeat_n('é', 250).collect();
        let truncated = truncate_for_error(&s, 200);
        assert_eq!(truncated.chars().count(), 201);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_nearest_matches_caps_count_and_length() {
        let many: Vec<String> = (0..50).map(|i| format!("fn_{i}")).collect();
        let capped = truncate_nearest_matches(many);
        assert_eq!(capped.len(), ERROR_LIST_LIMIT);
        assert_eq!(capped[0], "fn_0");
    }

    #[test]
    fn invalid_value_serializes_with_hint_when_set() {
        let err = ToolError::InvalidValue {
            field: "function".into(),
            value: "re:[broken".into(),
            accepted: vec!["<non-empty pattern>".into(), "re:<valid regex>".into()],
            hint: Some("regex parse error: unclosed character class".into()),
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error"], "invalid_value");
        assert_eq!(json["field"], "function");
        assert_eq!(json["hint"], "regex parse error: unclosed character class");
    }

    #[test]
    fn invalid_value_skips_hint_when_none() {
        let err = ToolError::InvalidValue {
            field: "process".into(),
            value: "pid:abc".into(),
            accepted: vec!["pid:NNN".into()],
            hint: None,
        };
        let json = serde_json::to_value(&err).unwrap();
        assert!(
            json.get("hint").is_none(),
            "expected `hint` to be omitted when None, got {json}"
        );
    }
}
