//! Structured error envelope returned by every MCP tool.
//!
//! Wherever the LLM has enough information to retry, prefer warning + recovery
//! over a hard failure. See spec §"Error handling" for the full table.

#![allow(dead_code)]

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum ToolError {
    FileNotFound { path: PathBuf },
    NotAProfile { path: PathBuf, details: String },
    UnsupportedProfileFormat { path: PathBuf, version: String },
    FunctionNotFound { function: String, nearest_matches: Vec<String> },
    FunctionAmbiguous { function: String, candidates: Vec<FunctionCandidate> },
    ThreadNotFound { thread: String, available_threads: Vec<ThreadRef> },
    ProcessNotFound { process: String, available_processes: Vec<ProcessRef> },
    OutOfBounds { original_range: [f64; 2], clamped_range: [f64; 2] },
    ProfileNotFound { profile_id: String },
    ProfileEvicted { profile_id: String, original_path: PathBuf },
    Internal { message: String },
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
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessRef {
    pub pid: u64,
    pub name: String,
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
}
