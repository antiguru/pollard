//! `source_for_function`: source code with per-line sample counts.

#![allow(dead_code)]

use crate::error::{FunctionCandidate, ToolError};
use crate::matching::{
    DidYouMean, FunctionMatcher, auto_promote_match, matcher_to_string, nearest_function_scored,
};
use crate::profile::Profile;
use crate::profile::raw::RawLib;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Args {
    pub function: String,
    pub module: Option<String>,
    pub with_samples: bool,
    pub whole_file: bool,
    /// When true, the matcher also considers DWARF inline frames — letting
    /// callers ask for source of an inlined function (e.g.
    /// `core::iter::Sum::sum`) rather than only the enclosing native one.
    /// Line attribution uses the inline frame's own (file, line).
    pub expand_inlines: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            function: String::new(),
            module: None,
            with_samples: true,
            whole_file: false,
            expand_inlines: false,
        }
    }
}

pub struct ResolvedSource {
    pub file: String,
    pub language: Option<String>,
    pub content: String,
}

/// Carries the bits needed to resolve source via samply-api `/source/v1`:
/// the lib's debug identity (debug_name + breakpad_id) and an address inside
/// the matched function (so the API can map back to the file).
#[derive(Debug, Clone)]
struct FetchContext {
    file: String,
    /// Library-relative offset of any sample inside the matched function.
    /// `None` means we can't make the API call and must rely on disk fallback.
    module_offset: Option<u32>,
    /// Lib metadata (debug_name + breakpad_id). Cloned so the context can
    /// outlive the borrow of `Profile`.
    lib: Option<RawLib>,
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
    /// Set when the requested function name didn't match exactly but the
    /// fuzzy ranker promoted a single high-confidence candidate. Surfaced so
    /// the caller can verify the substitution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<DidYouMean>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SourceLine {
    pub line: u32,
    pub samples: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples_pct: Option<f32>,
    pub code: String,
}

pub async fn source_for_function(
    profile: &Profile,
    args: &Args,
) -> Result<SourceListing, ToolError> {
    match source_for_function_inner(profile, &args.function, args, None).await {
        Err(ToolError::FunctionNotFound { .. }) => {
            // Try once more against an auto-promoted high-confidence fuzzy hit.
            let matcher =
                FunctionMatcher::new(&args.function).map_err(|e| ToolError::Internal {
                    message: e.to_string(),
                })?;
            let scored = nearest_function_scored(profile, &matcher);
            let Some(resolved) = auto_promote_match(&scored).map(str::to_owned) else {
                return Err(ToolError::FunctionNotFound {
                    function: args.function.clone(),
                    nearest_matches: scored.into_iter().map(|(n, _)| n).collect(),
                });
            };
            let dym = DidYouMean {
                needle: args.function.clone(),
                resolved: resolved.clone(),
            };
            source_for_function_inner(profile, &resolved, args, Some(dym)).await
        }
        other => other,
    }
}

async fn source_for_function_inner(
    profile: &Profile,
    function: &str,
    args: &Args,
    did_you_mean: Option<DidYouMean>,
) -> Result<SourceListing, ToolError> {
    let matcher = FunctionMatcher::new(function).map_err(|e| ToolError::Internal {
        message: e.to_string(),
    })?;
    let (ctx, _samples_per_line, _total) = attribute(
        profile,
        &matcher,
        args.module.as_deref(),
        args.expand_inlines,
    )?;
    let resolved = fetch_source(profile, &ctx).await?;
    let mut listing = build_listing(
        profile,
        function,
        args.module.as_deref(),
        resolved,
        args.with_samples,
        args.whole_file,
        args.expand_inlines,
    )?;
    listing.did_you_mean = did_you_mean;
    Ok(listing)
}

/// Record one matched frame's contribution to ambiguity tracking, source
/// fetch context, and per-line sample counts. Factored out so the native
/// path and the inline-chain path can share it.
#[allow(clippy::too_many_arguments)]
fn record_match(
    function_name: &str,
    module: Option<&str>,
    frame_file: Option<&str>,
    frame_line: Option<u32>,
    lib: Option<&RawLib>,
    address: Option<i64>,
    matched_pairs: &mut HashMap<(String, String), u64>,
    file: &mut Option<String>,
    ctx_lib: &mut Option<RawLib>,
    ctx_offset: &mut Option<u32>,
    samples_per_line: &mut HashMap<u32, u64>,
    total: &mut u64,
) {
    let key = (function_name.to_owned(), module.unwrap_or("").to_owned());
    *matched_pairs.entry(key).or_default() += 1;
    if file.is_none() {
        *file = frame_file.map(str::to_owned);
        *ctx_lib = lib.cloned();
    }
    // Capture the first usable address — needed to anchor the samply-api
    // `/source/v1` lookup. Frame addresses are stored as `i64`
    // (negative = unknown); samply-api expects a u32 library-relative offset.
    if ctx_offset.is_none()
        && let Some(addr) = address
        && let Ok(off) = u32::try_from(addr)
    {
        *ctx_offset = Some(off);
    }
    if let Some(line) = frame_line {
        *samples_per_line.entry(line).or_default() += 1;
        *total += 1;
    }
}

fn attribute(
    profile: &Profile,
    matcher: &FunctionMatcher,
    module_filter: Option<&str>,
    expand_inlines: bool,
) -> Result<(FetchContext, HashMap<u32, u64>, u64), ToolError> {
    let mut samples_per_line: HashMap<u32, u64> = HashMap::new();
    let mut total: u64 = 0;
    let mut file: Option<String> = None;
    let mut ctx_lib: Option<RawLib> = None;
    let mut ctx_offset: Option<u32> = None;
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
                let module = info.module_name;
                if let Some(m) = module_filter
                    && module != Some(m)
                {
                    continue;
                }

                // Native-frame match.
                if matcher.matches(info.function_name) {
                    record_match(
                        info.function_name,
                        module,
                        info.file,
                        info.line,
                        info.lib,
                        info.address,
                        &mut matched_pairs,
                        &mut file,
                        &mut ctx_lib,
                        &mut ctx_offset,
                        &mut samples_per_line,
                        &mut total,
                    );
                }

                // Inline-frame matches. Each inline entry brings its own
                // (file, line); the lib and address still come from the
                // enclosing native frame, since that's what samply-api needs
                // to resolve the file via `/source/v1`.
                if expand_inlines {
                    for inl in profile.inline_chain(handle, frame_idx) {
                        if !matcher.matches(&inl.function) {
                            continue;
                        }
                        record_match(
                            &inl.function,
                            module,
                            inl.file.as_deref(),
                            inl.line,
                            info.lib,
                            info.address,
                            &mut matched_pairs,
                            &mut file,
                            &mut ctx_lib,
                            &mut ctx_offset,
                            &mut samples_per_line,
                            &mut total,
                        );
                    }
                }
            }
        }
    }

    if matched_pairs.is_empty() {
        return Err(ToolError::FunctionNotFound {
            function: matcher_to_string(matcher),
            nearest_matches: nearest_function_scored(profile, matcher)
                .into_iter()
                .map(|(n, _)| n)
                .collect(),
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
    let ctx = FetchContext {
        file,
        module_offset: ctx_offset,
        lib: ctx_lib,
    };
    Ok((ctx, samples_per_line, total))
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

async fn fetch_source(_profile: &Profile, ctx: &FetchContext) -> Result<ResolvedSource, ToolError> {
    // Try samply-api `/source/v1` first — this is what samply itself uses to
    // surface registry / std-lib / build-system-relative paths in the Firefox
    // profiler UI. Falls back to local disk when the lib isn't reachable
    // (e.g. test fixtures with stripped paths) or the API can't resolve.
    if let Some(lib) = &ctx.lib
        && let Some(offset) = ctx.module_offset
        && let Some(resolved) = try_samply_source_api(lib, offset, &ctx.file).await
    {
        return Ok(resolved);
    }

    let path = std::path::Path::new(&ctx.file);
    if path.is_absolute() && path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| ToolError::Internal {
            message: e.to_string(),
        })?;
        Ok(ResolvedSource {
            file: ctx.file.clone(),
            language: guess_language(&ctx.file),
            content,
        })
    } else {
        Err(ToolError::Internal {
            message: format!("source file unavailable: {}", ctx.file),
        })
    }
}

fn guess_language(file: &str) -> Option<String> {
    let path = std::path::Path::new(file);
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => Some("rust".to_owned()),
        Some("c") => Some("c".to_owned()),
        Some("cpp" | "cc" | "cxx") => Some("cpp".to_owned()),
        Some("py") => Some("python".to_owned()),
        _ => None,
    }
}

/// Resolve `file` via wholesym + samply-api `/source/v1`. Returns `None` on
/// any failure (lib not loadable, address not in the symbol map, file not
/// permitted, JSON shape unexpected) so the caller can try the disk fallback.
async fn try_samply_source_api(
    lib: &RawLib,
    module_offset: u32,
    file: &str,
) -> Option<ResolvedSource> {
    use wholesym::{SymbolManager, SymbolManagerConfig};

    let debug_name = lib.debug_name.as_deref()?;
    let debug_id = lib.breakpad_id.as_deref()?;

    let config = SymbolManagerConfig::new().use_spotlight(true);
    let mut manager = SymbolManager::with_config(config);
    manager.add_known_library(crate::query::asm::build_library_info(lib));

    let request = serde_json::json!({
        "debugName": debug_name,
        "debugId": debug_id,
        "moduleOffset": format!("0x{module_offset:x}"),
        "file": file,
    })
    .to_string();
    let response_json = manager.query_json_api("/source/v1", &request).await;
    let value: serde_json::Value = serde_json::from_str(&response_json).ok()?;
    if value.get("error").is_some() {
        return None;
    }
    let source = value.get("source")?.as_str()?.to_owned();
    Some(ResolvedSource {
        file: file.to_owned(),
        language: guess_language(file),
        content: source,
    })
}

pub fn build_listing(
    profile: &Profile,
    function: &str,
    module: Option<&str>,
    resolved: ResolvedSource,
    _with_samples: bool,
    whole_file: bool,
    expand_inlines: bool,
) -> Result<SourceListing, ToolError> {
    // Re-attribute samples for this listing.
    let matcher = FunctionMatcher::new(function).map_err(|e| ToolError::Internal {
        message: e.to_string(),
    })?;
    let (_file, samples_per_line, total) = attribute(profile, &matcher, module, expand_inlines)?;

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
        did_you_mean: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;
    use crate::profile::raw::RawProfile;

    #[test]
    fn expand_inlines_matches_inline_function_with_its_own_line() {
        // linear_chain.json: a → b → c → d, 100 samples on `d`. Inject one
        // inline record on `d` mapping to a different file + line. With
        // expand_inlines, querying `leaf_inline` should resolve and return
        // a listing with samples attributed to the inline frame's line.
        use crate::profile::raw::InlineFrame;
        let mut raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let t = &mut raw.threads[0];
        t.inline_chains.resize_with(t.frame_table.length, Vec::new);
        t.inline_chains[3] = vec![InlineFrame {
            function: "leaf_inline".into(),
            file: Some("/tmp/leaf_inline.rs".into()),
            line: Some(2),
        }];
        let profile = Profile::from_raw(raw);

        let source = "fn outer() {}\nfn leaf_inline() { compute() }\n";
        let listing = build_listing(
            &profile,
            "leaf_inline",
            None,
            ResolvedSource {
                file: "/tmp/leaf_inline.rs".to_owned(),
                language: Some("rust".to_owned()),
                content: source.to_owned(),
            },
            true,
            true, // whole_file so we definitely include line 2
            true, // expand_inlines
        )
        .unwrap();

        // Line 2 (the inline frame's line) gets all 100 samples.
        let line2 = listing.lines.iter().find(|l| l.line == 2).expect("line 2");
        assert_eq!(line2.samples, 100);
        assert_eq!(listing.total_function_samples, 100);
    }

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
            false,
        )
        .unwrap_err();

        assert!(
            matches!(err, ToolError::FunctionNotFound { .. }),
            "expected FunctionNotFound, got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn ambiguous_substring_returns_function_ambiguous() {
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
        .await
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
