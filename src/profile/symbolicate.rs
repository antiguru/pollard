//! On-load symbolication pass using `wholesym`.
//!
//! For profiles recorded by samply on macOS, frame addresses are stored as
//! raw relative addresses (e.g. "0x4cb") without resolved function names.
//! This module walks the profile's frame table, detects unsymbolicated
//! frames, and resolves them via `wholesym` before `Profile::from_raw`
//! wraps the data.
//!
//! If a library cannot be found or wholesym fails to load it, the lib is
//! recorded with a [`LibSymbolicationStatus::LoadError`] in the per-lib
//! outcomes and frames belonging to that lib are left with their hex
//! names. Per-lib lookup counts are also tracked so callers can spot
//! "loaded the binary but every address missed" cases (e.g. a stale
//! rebuild where layout drifted).

use std::collections::HashMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;
use wholesym::{LookupAddress, SymbolManager, SymbolManagerConfig, SymbolMap};

use crate::profile::raw::{InlineFrame, RawLib, RawProfile};
use crate::profile::tables::SharedTables;

/// Returns true if the function name looks unsymbolicated. Treat any
/// `0x…` hex name as unsymbolicated, not just the literal `"0x0"` —
/// otherwise a profile whose hot frames all came back as raw addresses
/// (e.g. binary rebuilt between recording and analysis, see issue #80)
/// reports a near-zero unsymbolicated percentage.
pub(crate) fn is_unsymbolicated(name: &str) -> bool {
    name.is_empty() || name.starts_with("0x")
}

/// Outcome of attempting to symbolicate one library.
///
/// Surfaced through [`crate::session::ProfileSession`] so callers can see
/// *which* libraries failed to load and *how many* frames each one was
/// asked about. The pair "loaded ok, but every lookup returned None" is
/// the smoking gun for a stale-binary scenario.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LibSymbolicationOutcome {
    pub lib_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_name: Option<String>,
    /// Breakpad-formatted debug id, when samply recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_id: Option<String>,
    /// Path samply pointed at; reported back so users can immediately
    /// see which binary on disk pollard tried to open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub status: LibSymbolicationStatus,
    /// Frames whose addresses pointed at this lib and which symbolication
    /// attempted to resolve. Zero for libs we never reached.
    pub frames_attempted: u64,
    /// Subset of `frames_attempted` for which wholesym returned a symbol.
    /// `frames_attempted > 0 && frames_resolved == 0` is the canonical
    /// "stale binary loaded but layout drifted" signal.
    pub frames_resolved: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LibSymbolicationStatus {
    /// wholesym opened the binary. Per-frame lookups may still miss; the
    /// `frames_resolved` counter tells the rest of that story.
    Loaded,
    /// wholesym refused or failed to open the binary at the recorded
    /// path. Common causes: the binary was rebuilt and its debug_id no
    /// longer matches what the profile recorded, the file was moved or
    /// deleted, or it was never present on this host.
    LoadError { message: String },
    /// The lib record has no `path` and no `debug_path`, so there's
    /// nothing for wholesym to open.
    NoBinaryPath,
}

/// Filter outcomes down to entries worth surfacing in `summary` /
/// `describe_profile` by default: any non-`Loaded` lib, plus any lib
/// that loaded successfully yet returned zero resolutions for every
/// frame asked about (the canonical stale-binary signal — see issue
/// #80). Successfully-loaded libs with at least one resolved frame
/// are dropped to keep the diagnostic narrow.
pub fn problematic_outcomes(outcomes: &[LibSymbolicationOutcome]) -> Vec<LibSymbolicationOutcome> {
    outcomes
        .iter()
        .filter(|o| match &o.status {
            LibSymbolicationStatus::Loaded => o.frames_attempted > 0 && o.frames_resolved == 0,
            _ => true,
        })
        .cloned()
        .collect()
}

/// Per-lib accumulator used while walking threads. Converted into a
/// flat [`LibSymbolicationOutcome`] vec at the end of [`symbolicate`].
struct OutcomeAccum {
    lib_name: String,
    debug_name: Option<String>,
    debug_id: Option<String>,
    path: Option<String>,
    status: LibSymbolicationStatus,
    frames_attempted: u64,
    frames_resolved: u64,
}

impl OutcomeAccum {
    fn from_lib(lib: &RawLib, status: LibSymbolicationStatus) -> Self {
        Self {
            lib_name: lib.name.clone().unwrap_or_default(),
            debug_name: lib.debug_name.clone(),
            debug_id: lib.breakpad_id.clone(),
            path: lib.path.clone().or_else(|| lib.debug_path.clone()),
            status,
            frames_attempted: 0,
            frames_resolved: 0,
        }
    }

    fn into_outcome(self) -> LibSymbolicationOutcome {
        LibSymbolicationOutcome {
            lib_name: self.lib_name,
            debug_name: self.debug_name,
            debug_id: self.debug_id,
            path: self.path,
            status: self.status,
            frames_attempted: self.frames_attempted,
            frames_resolved: self.frames_resolved,
        }
    }
}

/// Stable identifier for cross-process aggregation. Multiple processes
/// may reference what is in fact the same binary (same `debug_id`); we
/// fold their counters together. Falls back to `(name, path)` when no
/// `debug_id` is present.
fn outcome_key(lib: &RawLib) -> String {
    if let Some(id) = lib.breakpad_id.as_deref() {
        return id.to_owned();
    }
    let name = lib.name.as_deref().unwrap_or("");
    let path = lib
        .path
        .as_deref()
        .or(lib.debug_path.as_deref())
        .unwrap_or("");
    format!("{name}|{path}")
}

/// Symbolicate a `RawProfile` in-place.
///
/// Best-effort: any lib that wholesym cannot load is recorded with a
/// `LoadError` outcome and its frames are left as hex.
pub async fn symbolicate(
    raw: &mut RawProfile,
) -> Result<Vec<LibSymbolicationOutcome>, crate::error::ToolError> {
    // `use_spotlight` is macOS-only — it asks Spotlight for adjacent .dSYM
    // bundles. Enabling it on Linux pushes wholesym into a macOS-shaped
    // resolution path that ends in a dyld-shared-cache read; every Linux
    // module then fails symbolication with "could not load symbols ...
    // /System/Library/dyld/dyld_shared_cache_x86_64".
    let config = SymbolManagerConfig::new().use_spotlight(cfg!(target_os = "macos"));
    let symbol_manager = SymbolManager::with_config(config);

    let mut outcomes: HashMap<String, OutcomeAccum> = HashMap::new();
    symbolicate_tables(&symbol_manager, &mut raw.shared, &mut outcomes).await;

    let mut flat: Vec<LibSymbolicationOutcome> = outcomes
        .into_values()
        .map(OutcomeAccum::into_outcome)
        .collect();
    // Stable order: lib_name asc, then debug_id asc as tiebreak. Avoids
    // shuffle from HashMap iteration so snapshot tests stay deterministic.
    flat.sort_by(|a, b| {
        a.lib_name
            .cmp(&b.lib_name)
            .then_with(|| a.debug_id.cmp(&b.debug_id))
    });

    Ok(flat)
}

async fn symbolicate_tables(
    symbol_manager: &SymbolManager,
    t: &mut SharedTables,
    outcomes: &mut HashMap<String, OutcomeAccum>,
) {
    // Collect work: (frame_idx, func_idx, lib_idx, address)
    // We do this in a preliminary pass to avoid borrow conflicts.
    let mut work: Vec<(usize, usize, usize, u32)> = Vec::new();

    for frame_idx in 0..t.frames.len() {
        let Some(addr) = t.frames.address[frame_idx] else {
            continue; // non-native frame
        };
        let func_idx = t.frames.func[frame_idx];
        let name = t.strings.get(t.funcs.name[func_idx]).unwrap_or("");
        if !is_unsymbolicated(name) {
            continue; // already symbolicated
        }

        // Resolve lib through the frame's library column
        let Some(lib_idx) = t.frames.lib[frame_idx] else {
            continue;
        };

        work.push((frame_idx, func_idx, lib_idx, addr));
    }

    if work.is_empty() {
        return;
    }

    // Pre-size the parallel inline-chain table so per-frame writes below
    // can index directly. Empty Vec for frames without inline records.
    if t.inline_chains.len() < t.frames.len() {
        t.inline_chains.resize_with(t.frames.len(), Vec::new);
    }

    // Cache: lib_index → SymbolMap (or None if we failed to load it).
    // Library indices are profile-global, so each library loads once.
    let mut symbol_map_cache: HashMap<usize, Option<SymbolMap>> = HashMap::new();

    // Load symbol maps for all libs needed by the profile.
    let lib_indices_needed: Vec<usize> = {
        let mut seen = std::collections::HashSet::new();
        work.iter()
            .filter_map(
                |&(_, _, li, _)| {
                    if seen.insert(li) { Some(li) } else { None }
                },
            )
            .collect()
    };

    for &lib_idx in &lib_indices_needed {
        if symbol_map_cache.contains_key(&lib_idx) {
            continue;
        }
        let raw_lib = t.libs.get(lib_idx);
        let (map, load_status) = load_symbol_map_for_lib(symbol_manager, raw_lib).await;
        // Record the load status in the per-lib outcome the first time
        // we see this lib. Subsequent attempts (e.g. reused across
        // threads) just inherit the cached status.
        if let Some(lib) = raw_lib {
            outcomes
                .entry(outcome_key(lib))
                .or_insert_with(|| OutcomeAccum::from_lib(lib, load_status));
        }
        symbol_map_cache.insert(lib_idx, map);
    }

    // Apply symbolication results.
    for (frame_idx, func_idx, lib_idx, addr) in work {
        let raw_lib = t.libs.get(lib_idx);
        // We only count attempts and resolutions for libs we have a
        // record for — frames whose lib is missing from the table can't
        // be attributed and would distort the per-lib counters.
        let outcome_slot = raw_lib.map(outcome_key);

        let map = match symbol_map_cache.get(&lib_idx).and_then(|o| o.as_ref()) {
            Some(m) => m,
            None => {
                // No map → the load failed; we still count the attempt
                // so the diagnostic shows "asked about N frames, never
                // resolved any" rather than vanishing.
                if let Some(key) = outcome_slot.as_ref()
                    && let Some(acc) = outcomes.get_mut(key)
                {
                    acc.frames_attempted += 1;
                }
                continue;
            }
        };

        if let Some(key) = outcome_slot.as_ref()
            && let Some(acc) = outcomes.get_mut(key)
        {
            acc.frames_attempted += 1;
        }

        let lookup = map.lookup(LookupAddress::Relative(addr)).await;
        let Some(addr_info) = lookup else { continue };

        if let Some(key) = outcome_slot.as_ref()
            && let Some(acc) = outcomes.get_mut(key)
        {
            acc.frames_resolved += 1;
        }

        // Use the OUTER inline frame for both name and source attribution.
        //
        // samply-symbols documents `frames` as innermost-first: `frames.first()`
        // is the deepest inlined callee (e.g. `core::iter::Sum::sum`),
        // `frames.last()` is the enclosing function (e.g. `simd_cols_1st`).
        // Using `frames.last()` means:
        //   - top_functions/call_tree see the user's function, not the inlined
        //     stdlib call.
        //   - source_for_function gets a `line` that points at the call-site
        //     line within the user's source file rather than a line inside the
        //     inlined callee's source.
        //
        // Inline-frame expansion (issue #7) is the long-term answer for
        // letting users *also* drill into the inlined callee individually.
        let outer = addr_info.frames.as_ref().and_then(|fs| fs.last());

        let resolved_name = outer
            .and_then(|f| f.function.as_deref())
            .unwrap_or(&addr_info.symbol.name);
        let new_name_idx = t.strings.intern(resolved_name);
        t.funcs.name[func_idx] = new_name_idx;

        if let Some(frame) = outer {
            if let Some(file) = frame
                .file_path
                .as_ref()
                .map(|p| p.display_path().to_owned())
            {
                let file_idx = t.strings.intern(&file);
                t.funcs.file_name[func_idx] = Some(file_idx);
            }
            if let Some(line) = frame.line_number {
                t.frames.line[frame_idx] = Some(line);
            }
        }

        // Capture the inner inline frames (everything except `frames.last()`,
        // which is the enclosing native function we already wrote above).
        // `addr_info.frames` is innermost-first per samply-symbols, so
        // `frames[0]` is the deepest inlined callee and we slice off the
        // outer entry from the tail.
        if let Some(frames) = addr_info.frames.as_ref()
            && frames.len() > 1
        {
            let inner = &frames[..frames.len() - 1];
            t.inline_chains[frame_idx] = inner
                .iter()
                .map(|f| InlineFrame {
                    function: f.function.clone().unwrap_or_default(),
                    file: f.file_path.as_ref().map(|p| p.display_path().to_owned()),
                    line: f.line_number,
                })
                .collect();
        }
    }
}

async fn load_symbol_map_for_lib(
    symbol_manager: &SymbolManager,
    raw_lib: Option<&RawLib>,
) -> (Option<SymbolMap>, LibSymbolicationStatus) {
    let Some(lib) = raw_lib else {
        return (None, LibSymbolicationStatus::NoBinaryPath);
    };

    let Some(path_str) = lib.path.as_deref().or(lib.debug_path.as_deref()) else {
        return (None, LibSymbolicationStatus::NoBinaryPath);
    };
    let path = Path::new(path_str);

    // Derive a MultiArchDisambiguator from the arch field if present (macOS fat binaries).
    let disambiguator = lib
        .arch
        .as_ref()
        .map(|arch| wholesym::MultiArchDisambiguator::Arch(arch.clone()));

    match symbol_manager
        .load_symbol_map_for_binary_at_path(path, disambiguator)
        .await
    {
        Ok(map) => (Some(map), LibSymbolicationStatus::Loaded),
        Err(e) => (
            None,
            LibSymbolicationStatus::LoadError {
                message: e.to_string(),
            },
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_unsymbolicated_catches_nonzero_hex_names() {
        // The original predicate (`name == "0x0"`) only flagged the
        // literal zero address, leaving real hex names like
        // `"0xb9e0452"` counted as symbolicated. After issue #80 we
        // unify on the broader `starts_with("0x")` rule so the
        // diagnostic surfaces stale-binary scenarios honestly.
        assert!(is_unsymbolicated(""));
        assert!(is_unsymbolicated("0x0"));
        assert!(is_unsymbolicated("0xb9e0452"));
        assert!(is_unsymbolicated("0x7f2eda66bcf7"));
        assert!(!is_unsymbolicated("memcpy"));
        assert!(!is_unsymbolicated("<core::iter::Sum>::sum"));
    }

    fn outcome(
        lib_name: &str,
        status: LibSymbolicationStatus,
        attempted: u64,
        resolved: u64,
    ) -> LibSymbolicationOutcome {
        LibSymbolicationOutcome {
            lib_name: lib_name.to_owned(),
            debug_name: None,
            debug_id: None,
            path: None,
            status,
            frames_attempted: attempted,
            frames_resolved: resolved,
        }
    }

    #[test]
    fn problematic_outcomes_keeps_load_errors() {
        let outcomes = vec![outcome(
            "missing",
            LibSymbolicationStatus::LoadError {
                message: "boom".into(),
            },
            0,
            0,
        )];
        let kept = problematic_outcomes(&outcomes);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].lib_name, "missing");
    }

    #[test]
    fn problematic_outcomes_keeps_loaded_with_zero_resolutions() {
        // Stale-binary signature: wholesym opened the file, every
        // address lookup missed. `problematic_outcomes` must surface
        // it even though `status == Loaded`.
        let outcomes = vec![outcome("stale", LibSymbolicationStatus::Loaded, 100, 0)];
        let kept = problematic_outcomes(&outcomes);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].frames_attempted, 100);
        assert_eq!(kept[0].frames_resolved, 0);
    }

    #[test]
    fn problematic_outcomes_drops_healthy_loaded_libs() {
        // Loaded with at least one resolved frame is the no-news case;
        // those libs should not bloat the diagnostic.
        let outcomes = vec![
            outcome("happy", LibSymbolicationStatus::Loaded, 50, 50),
            outcome("partial", LibSymbolicationStatus::Loaded, 50, 1),
        ];
        assert!(problematic_outcomes(&outcomes).is_empty());
    }

    #[test]
    fn problematic_outcomes_keeps_no_binary_path() {
        let outcomes = vec![outcome(
            "orphan",
            LibSymbolicationStatus::NoBinaryPath,
            0,
            0,
        )];
        let kept = problematic_outcomes(&outcomes);
        assert_eq!(kept.len(), 1);
    }

    #[tokio::test]
    async fn symbolicate_records_load_error_for_missing_binary() {
        // Synthesize a profile with a hex-named frame pointing at a
        // lib whose `path` does not exist. Symbolicate must record a
        // `LoadError` outcome and count one attempted, zero resolved.
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [{
                "name": "ghost",
                "path": "/definitely/not/a/real/path/ghost",
                "debugName": "ghost",
                "debugPath": "/definitely/not/a/real/path/ghost",
                "breakpadId": "DEADBEEFDEADBEEFDEADBEEFDEADBEEF0",
                "codeId": null,
                "arch": null
            }],
            "threads": [{
                "name": "Main",
                "processName": "test",
                "tid": 1,
                "pid": 1,
                "registerTime": 0.0,
                "stringArray": ["0xdead"],
                "frameTable": {"length": 1, "address": [57005], "func": [0], "category": [0], "subcategory": [0], "line": [null], "column": [null], "nativeSymbol": [null]},
                "stackTable": {"length": 1, "frame": [0], "category": [0], "subcategory": [0], "prefix": [null]},
                "samples": {"length": 1, "stack": [0], "time": [0.0]},
                "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [0], "fileName": [null], "lineNumber": [null], "columnNumber": [null]},
                "resourceTable": {"length": 1, "lib": [0], "name": [0], "host": [null], "type": [1]},
                "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
            }]
        }"#;
        let mut raw: RawProfile = serde_json::from_str(json).unwrap();
        let outcomes = symbolicate(&mut raw).await.unwrap();
        assert_eq!(
            outcomes.len(),
            1,
            "expected one lib outcome, got {outcomes:?}"
        );
        let o = &outcomes[0];
        assert_eq!(o.lib_name, "ghost");
        assert!(matches!(o.status, LibSymbolicationStatus::LoadError { .. }));
        assert_eq!(o.frames_attempted, 1);
        assert_eq!(o.frames_resolved, 0);
        // Surfaced as problematic.
        assert_eq!(problematic_outcomes(&outcomes).len(), 1);
    }
}
