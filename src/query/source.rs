//! `source_for_function`: source code with per-line sample counts.

#![allow(dead_code)]

use crate::error::{FunctionCandidate, ToolError};
use crate::matching::{FunctionMatcher, matcher_to_string, nearest_function_names};
use crate::profile::Profile;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Args {
    pub function: String,
    pub module: Option<String>,
    pub with_samples: bool,
    pub whole_file: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            function: String::new(),
            module: None,
            with_samples: true,
            whole_file: false,
        }
    }
}

pub struct ResolvedSource {
    pub file: String,
    pub language: Option<String>,
    pub content: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SourceListing {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub total_function_samples: u64,
    pub line_range: [u32; 2],
    pub lines: Vec<SourceLine>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SourceLine {
    pub line: u32,
    pub samples: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples_pct: Option<f32>,
    pub code: String,
}

pub fn source_for_function(profile: &Profile, args: &Args) -> Result<SourceListing, ToolError> {
    let matcher = FunctionMatcher::new(&args.function).map_err(|e| ToolError::Internal {
        message: e.to_string(),
    })?;
    let (file, _samples_per_line, _total) = attribute(profile, &matcher, args.module.as_deref())?;
    let resolved = fetch_source(profile, &file)?;
    build_listing(
        profile,
        &args.function,
        args.module.as_deref(),
        resolved,
        args.with_samples,
        args.whole_file,
    )
}

fn attribute(
    profile: &Profile,
    matcher: &FunctionMatcher,
    module_filter: Option<&str>,
) -> Result<(String, HashMap<u32, u64>, u64), ToolError> {
    let mut samples_per_line: HashMap<u32, u64> = HashMap::new();
    let mut total: u64 = 0;
    let mut file: Option<String> = None;
    // Track which distinct (function, module) pairs the matcher hit, so we
    // can surface ambiguity instead of silently merging samples across
    // unrelated functions that happen to share a substring.
    let mut matched_pairs: HashMap<(String, String), u64> = HashMap::new();

    for thread in profile.threads() {
        let handle = thread.handle();
        let raw = profile.raw_thread(handle);
        for &stack_opt in &raw.samples.stack {
            let Some(stack_idx) = stack_opt else { continue };
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                let Some(info) = profile.frame_info(handle, frame_idx) else {
                    continue;
                };
                if !matcher.matches(info.function_name) {
                    continue;
                }
                if let Some(m) = module_filter
                    && info.module_name != Some(m)
                {
                    continue;
                }
                let key = (
                    info.function_name.to_owned(),
                    info.module_name.unwrap_or("").to_owned(),
                );
                *matched_pairs.entry(key).or_default() += 1;
                if file.is_none() {
                    file = info.file.map(str::to_owned);
                }
                if let Some(line) = info.line {
                    *samples_per_line.entry(line).or_default() += 1;
                    total += 1;
                }
            }
        }
    }

    if matched_pairs.is_empty() {
        return Err(ToolError::FunctionNotFound {
            function: matcher_to_string(matcher),
            nearest_matches: nearest_function_names(profile, matcher),
        });
    }
    if matched_pairs.len() > 1 {
        return Err(ToolError::FunctionAmbiguous {
            function: matcher_to_string(matcher),
            candidates: rank_candidates(matched_pairs),
        });
    }
    let file = file.ok_or_else(|| ToolError::Internal {
        message: format!(
            "function `{}` exists in profile but has no source-line information \
             (DWARF/dSYM not available — try rebuilding with debug info or pointing \
             to the .dSYM bundle)",
            matcher_to_string(matcher),
        ),
    })?;
    Ok((file, samples_per_line, total))
}

/// Sort matched (function, module) pairs by sample count desc and convert
/// into the API-facing [`FunctionCandidate`] shape.
fn rank_candidates(pairs: HashMap<(String, String), u64>) -> Vec<FunctionCandidate> {
    let mut entries: Vec<((String, String), u64)> = pairs.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.0.cmp(&b.0.0)));
    entries
        .into_iter()
        .map(|((function, module), _)| FunctionCandidate { function, module })
        .collect()
}

fn fetch_source(_profile: &Profile, file: &str) -> Result<ResolvedSource, ToolError> {
    let path = std::path::Path::new(file);
    if path.is_absolute() && path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| ToolError::Internal {
            message: e.to_string(),
        })?;
        let language = match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => Some("rust".to_owned()),
            Some("c") => Some("c".to_owned()),
            Some("cpp" | "cc" | "cxx") => Some("cpp".to_owned()),
            Some("py") => Some("python".to_owned()),
            _ => None,
        };
        Ok(ResolvedSource {
            file: file.to_owned(),
            language,
            content,
        })
    } else {
        Err(ToolError::Internal {
            message: format!("source file unavailable: {}", file),
        })
    }
}

pub fn build_listing(
    profile: &Profile,
    function: &str,
    module: Option<&str>,
    resolved: ResolvedSource,
    _with_samples: bool,
    whole_file: bool,
) -> Result<SourceListing, ToolError> {
    // Re-attribute samples for this listing.
    let matcher = FunctionMatcher::new(function).map_err(|e| ToolError::Internal {
        message: e.to_string(),
    })?;
    let (_file, samples_per_line, total) = attribute(profile, &matcher, module)?;

    let total_lines: Vec<(u32, String)> = resolved
        .content
        .lines()
        .enumerate()
        .map(|(i, code)| ((i + 1) as u32, code.to_owned()))
        .collect();

    // For !whole_file, restrict to min(samples_per_line.keys) ± 5 lines.
    let (range_start, range_end) = if whole_file || samples_per_line.is_empty() {
        (
            total_lines.first().map(|(n, _)| *n).unwrap_or(0),
            total_lines.last().map(|(n, _)| *n).unwrap_or(0),
        )
    } else {
        let min_l = *samples_per_line.keys().min().unwrap();
        let max_l = *samples_per_line.keys().max().unwrap();
        (min_l.saturating_sub(5), max_l.saturating_add(5))
    };

    let total_f = total.max(1) as f32;
    let lines: Vec<SourceLine> = total_lines
        .into_iter()
        .filter(|(n, _)| *n >= range_start && *n <= range_end)
        .map(|(n, code)| {
            let s = samples_per_line.get(&n).copied().unwrap_or(0);
            SourceLine {
                line: n,
                samples: s,
                samples_pct: if total > 0 {
                    Some(100.0 * s as f32 / total_f)
                } else {
                    None
                },
                code,
            }
        })
        .collect();

    let line_range = if let (Some(first), Some(last)) = (lines.first(), lines.last()) {
        [first.line, last.line]
    } else {
        [0, 0]
    };

    Ok(SourceListing {
        function: function.to_owned(),
        module: module.map(str::to_owned),
        file: resolved.file,
        language: resolved.language,
        total_function_samples: total,
        line_range,
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;
    use crate::profile::raw::RawProfile;

    #[test]
    fn returns_per_line_samples() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/source_attribution.json"))
                .unwrap();
        let profile = Profile::from_raw(raw);

        let source =
            "fn process_request() {\n    let x = parse();\n    validate(x);\n    return;\n}\n";
        let listing = build_listing(
            &profile,
            "process_request",
            None,
            ResolvedSource {
                file: "src/server.rs".to_owned(),
                language: Some("rust".to_owned()),
                content: source.to_owned(),
            },
            true,
            false,
        )
        .unwrap();

        // Lines 2 (parse) and 3 (validate) should have sample attributions per the fixture.
        assert!(listing.lines.iter().any(|l| l.line == 3 && l.samples > 0));
    }

    #[test]
    fn function_present_without_line_info_returns_internal_not_function_not_found() {
        // two_functions.json has frames for `hot` but no per-frame line numbers.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let profile = Profile::from_raw(raw);

        let err = build_listing(
            &profile,
            "hot",
            None,
            ResolvedSource {
                file: "x.rs".to_owned(),
                language: None,
                content: "// dummy\n".to_owned(),
            },
            true,
            false,
        )
        .unwrap_err();

        match err {
            ToolError::Internal { message } => {
                assert!(
                    message.contains("source-line information"),
                    "unexpected message: {}",
                    message
                );
            }
            other => panic!("expected Internal, got {:?}", other),
        }
    }

    #[test]
    fn truly_absent_function_returns_function_not_found() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let profile = Profile::from_raw(raw);

        let err = build_listing(
            &profile,
            "definitely_not_in_profile",
            None,
            ResolvedSource {
                file: "x.rs".to_owned(),
                language: None,
                content: "// dummy\n".to_owned(),
            },
            true,
            false,
        )
        .unwrap_err();

        assert!(
            matches!(err, ToolError::FunctionNotFound { .. }),
            "expected FunctionNotFound, got {:?}",
            err
        );
    }

    #[test]
    fn ambiguous_substring_returns_function_ambiguous() {
        // two_functions.json has both `hot` and `cold` — substring "o" hits
        // both. Without ambiguity detection, samples would silently merge
        // across the two functions.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let profile = Profile::from_raw(raw);

        let err = source_for_function(
            &profile,
            &Args {
                function: "o".to_owned(),
                ..Default::default()
            },
        )
        .unwrap_err();

        match err {
            ToolError::FunctionAmbiguous {
                function,
                candidates,
            } => {
                assert_eq!(function, "o");
                let names: Vec<&str> = candidates.iter().map(|c| c.function.as_str()).collect();
                assert!(names.contains(&"hot"), "expected `hot` in {names:?}");
                assert!(names.contains(&"cold"), "expected `cold` in {names:?}");
            }
            other => panic!("expected FunctionAmbiguous, got {other:?}"),
        }
    }
}
