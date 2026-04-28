//! On-load symbolication pass using `wholesym`.
//!
//! For profiles recorded by samply on macOS, frame addresses are stored as
//! raw relative addresses (e.g. "0x4cb") without resolved function names.
//! This module walks every thread, detects unsymbolicated frames, and
//! resolves them via `wholesym` before `Profile::from_raw` wraps the data.
//!
//! If a library cannot be found or wholesym fails to load it, the lib is
//! skipped silently (single stderr line) and those frames remain with their
//! hex-address names.

use std::collections::HashMap;
use std::path::Path;

use wholesym::{LookupAddress, SymbolManager, SymbolManagerConfig, SymbolMap};

use crate::profile::raw::{RawProfile, RawThread};

/// Returns true if the function name looks unsymbolicated.
fn is_unsymbolicated(name: &str) -> bool {
    name.is_empty() || name.starts_with("0x") || name == "0x0"
}

/// Add or find a string in the string array, returning its index.
fn intern_string(string_array: &mut Vec<String>, s: &str) -> usize {
    if let Some(pos) = string_array.iter().position(|x| x == s) {
        return pos;
    }
    let idx = string_array.len();
    string_array.push(s.to_owned());
    idx
}

/// Symbolicate all threads in a `RawProfile` in-place.
///
/// Best-effort: any lib that wholesym cannot load is skipped silently.
/// Returns `Ok(())` always (errors are logged to stderr per-lib).
pub async fn symbolicate(raw: &mut RawProfile) -> Result<(), crate::error::ToolError> {
    let config = SymbolManagerConfig::new().use_spotlight(true);
    let symbol_manager = SymbolManager::with_config(config);

    // Process top-level threads (they share raw.libs for lib lookup)
    symbolicate_threads(&symbol_manager, &mut raw.threads, &raw.libs).await;

    // Process sub-process threads (each process has its own libs table)
    for process in &mut raw.processes {
        symbolicate_threads(&symbol_manager, &mut process.threads, &process.libs).await;
    }

    Ok(())
}

async fn symbolicate_threads(
    symbol_manager: &SymbolManager,
    threads: &mut [RawThread],
    libs: &[crate::profile::raw::RawLib],
) {
    // Cache: lib_index → SymbolMap (or None if we failed to load it)
    let mut symbol_map_cache: HashMap<usize, Option<SymbolMap>> = HashMap::new();

    for thread in threads.iter_mut() {
        symbolicate_thread(symbol_manager, thread, libs, &mut symbol_map_cache).await;
    }
}

async fn symbolicate_thread(
    symbol_manager: &SymbolManager,
    thread: &mut RawThread,
    libs: &[crate::profile::raw::RawLib],
    symbol_map_cache: &mut HashMap<usize, Option<SymbolMap>>,
) {
    // Collect work: (frame_idx, func_idx, lib_idx, address)
    // We do this in a preliminary pass to avoid borrow conflicts.
    let mut work: Vec<(usize, usize, usize, u32)> = Vec::new();

    for frame_idx in 0..thread.frame_table.length {
        let addr = thread.frame_table.address[frame_idx];
        if addr < 0 {
            continue; // non-native frame
        }
        let func_idx = thread.frame_table.func[frame_idx];
        let name_str_idx = thread.func_table.name[func_idx];
        let name = thread
            .string_array
            .get(name_str_idx)
            .map(String::as_str)
            .unwrap_or("");
        if !is_unsymbolicated(name) {
            continue; // already symbolicated
        }

        // Resolve lib through resource table
        let resource_idx = thread.func_table.resource[func_idx];
        if resource_idx < 0 {
            continue;
        }
        let resource_idx = resource_idx as usize;
        let lib_idx = match thread.resource_table.lib.get(resource_idx).and_then(|o| *o) {
            Some(li) => li,
            None => continue,
        };

        work.push((frame_idx, func_idx, lib_idx, addr as u32));
    }

    if work.is_empty() {
        return;
    }

    // Load symbol maps for all libs needed by this thread.
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
        let raw_lib = libs.get(lib_idx);
        let map = load_symbol_map_for_lib(symbol_manager, raw_lib).await;
        symbol_map_cache.insert(lib_idx, map);
    }

    // Apply symbolication results.
    for (frame_idx, func_idx, lib_idx, addr) in work {
        let map = match symbol_map_cache.get(&lib_idx).and_then(|o| o.as_ref()) {
            Some(m) => m,
            None => continue,
        };

        let lookup = map.lookup(LookupAddress::Relative(addr)).await;
        let Some(addr_info) = lookup else { continue };

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
        let new_name_idx = intern_string(&mut thread.string_array, resolved_name);
        thread.func_table.name[func_idx] = new_name_idx;

        if let Some(frame) = outer {
            if let Some(file) = frame
                .file_path
                .as_ref()
                .map(|p| p.display_path().to_owned())
            {
                let file_idx = intern_string(&mut thread.string_array, &file);
                thread.func_table.file_name[func_idx] = Some(file_idx);
            }
            if let Some(line) = frame.line_number {
                thread.frame_table.line[frame_idx] = Some(line);
            }
        }
    }
}

async fn load_symbol_map_for_lib(
    symbol_manager: &SymbolManager,
    raw_lib: Option<&crate::profile::raw::RawLib>,
) -> Option<SymbolMap> {
    let lib = raw_lib?;

    // Prefer path, fall back to debug_path.
    let path_str = lib.path.as_deref().or(lib.debug_path.as_deref())?;
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
        Ok(map) => Some(map),
        Err(e) => {
            eprintln!("pollard: could not load symbols for {:?}: {}", path_str, e);
            None
        }
    }
}
