//! `asm_for_function`: disassembly with per-instruction sample counts.
//!
//! # Address conventions
//!
//! Frame addresses in the Firefox profile format are **relative** lib offsets
//! (stored as i64, with -1 meaning "non-native").  `nativeSymbols.address` is
//! also a relative lib offset.  The samply-api `/asm/v1` endpoint likewise
//! expects a `startAddress` that is a library-relative offset.  So no
//! absolute-to-relative conversion is needed here.
//!
//! # dyld shared cache
//!
//! On macOS, system libraries (e.g. `/usr/lib/libSystem.B.dylib`) live in the
//! dyld shared cache and typically have no on-disk bytes at their usual path.
//! wholesym handles those via its Spotlight integration, but the underlying
//! `read_bytes_at_relative_address` call that samply-api makes will fail for
//! those images.  When that happens, `asm_for_function` returns
//! `ToolError::Internal` with a clear message.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::matching::{FunctionMatcher, matcher_to_string, nearest_function_names};
use crate::profile::Profile;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;
use wholesym::{SymbolManager, SymbolManagerConfig};

#[derive(Debug, Default)]
pub struct Args {
    pub function: String,
    pub module: Option<String>,
    pub with_samples: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AsmListing {
    pub function: String,
    pub module: Option<String>,
    pub start_address: String,
    pub size: String,
    pub arch: String,
    pub instructions: Vec<AsmInstruction>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AsmInstruction {
    pub offset: u32,
    pub asm: String,
    pub samples: u64,
}

// ─── function resolution ────────────────────────────────────────────────────

/// Everything we need about a function to call the disassembler.
struct FunctionLocation {
    /// Lib-relative start address of the function.
    start_rel: u32,
    /// Byte size of the function (best estimate).
    size_bytes: u32,
    /// Index into `profile.raw.libs` (or the thread's process libs).
    lib_idx: usize,
    /// Frame relative addresses (lib-relative) that fell inside this function,
    /// with per-address sample counts for attribution.
    frame_counts: HashMap<u32, u64>,
}

/// Walk every sample/frame and resolve the function to a `FunctionLocation`.
///
/// Returns `None` (indicating "function not found") if no matching frame is
/// found.  If found, returns the location and accumulated per-address sample
/// counts.
fn resolve_function(
    profile: &Profile,
    matcher: &FunctionMatcher,
    module_filter: Option<&str>,
) -> Option<FunctionLocation> {
    // --- pass 1: collect matching (frame_idx, native_symbol_idx?, rel_addr, lib_idx) per thread

    // We aggregate globally across threads.
    // lib_idx → (start_rel, size) from nativeSymbols, if available.
    let mut native_loc: Option<(u32, u32, usize)> = None; // (start, size, lib_idx)
    let mut frame_counts: HashMap<u32, u64> = HashMap::new();
    let mut addr_min: Option<u32> = None;
    let mut addr_max: Option<u32> = None;
    let mut found_lib_idx: Option<usize> = None;

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

                // Get the relative address for this frame.
                let Some(rel_addr_i64) = info.address else {
                    continue;
                };
                if rel_addr_i64 < 0 {
                    continue;
                }
                let rel_addr = rel_addr_i64 as u32;

                // Resolve lib_idx via frame → func → resource → lib.
                let func_idx = raw.frame_table.func[frame_idx];
                let resource_idx = raw.func_table.resource[func_idx];
                if resource_idx < 0 {
                    continue;
                }
                let lib_idx = match raw
                    .resource_table
                    .lib
                    .get(resource_idx as usize)
                    .and_then(|o| *o)
                {
                    Some(li) => li,
                    None => continue,
                };

                // Try to get start/size from nativeSymbols.
                if native_loc.is_none()
                    && let Some(ns) = &raw.native_symbols
                {
                    let native_sym_idx = raw
                        .frame_table
                        .native_symbol
                        .get(frame_idx)
                        .and_then(|o| *o);
                    if let Some(ns_idx) = native_sym_idx {
                        let ns_addr = ns.address.get(ns_idx).copied().unwrap_or(-1);
                        let ns_size = ns.function_size.get(ns_idx).copied().flatten().unwrap_or(0);
                        if ns_addr >= 0 {
                            native_loc = Some((ns_addr as u32, ns_size as u32, lib_idx));
                        }
                    }
                }

                // Track min/max address for size estimation fallback.
                addr_min = Some(addr_min.map_or(rel_addr, |m: u32| m.min(rel_addr)));
                addr_max = Some(addr_max.map_or(rel_addr, |m: u32| m.max(rel_addr)));
                if found_lib_idx.is_none() {
                    found_lib_idx = Some(lib_idx);
                }

                *frame_counts.entry(rel_addr).or_default() += 1;
            }
        }
    }

    if frame_counts.is_empty() && native_loc.is_none() {
        return None;
    }

    let lib_idx = native_loc.map(|(_, _, li)| li).or(found_lib_idx)?;

    let (start_rel, size_bytes) = if let Some((start, size, _)) = native_loc {
        (start, size.max(1))
    } else {
        // Fallback: use span of observed addresses.
        let min = addr_min.unwrap_or(0);
        let max = addr_max.unwrap_or(min);
        // Add a small overread so the last instruction can be decoded.
        let estimated = (max - min).saturating_add(16);
        (min, estimated)
    };

    Some(FunctionLocation {
        start_rel,
        size_bytes,
        lib_idx,
        frame_counts,
    })
}

// ─── disassembly via wholesym/samply-api ────────────────────────────────────

/// A single decoded instruction with its byte length derived from the response.
struct DecodedInstr {
    /// Byte offset from the function's `start_rel`.
    offset: u32,
    /// Byte length of this instruction (derived from adjacent offsets).
    len: u32,
    /// Human-readable disassembly text.
    text: String,
}

async fn disassemble(
    lib: &crate::profile::raw::RawLib,
    start_rel: u32,
    size_bytes: u32,
) -> Result<(Vec<DecodedInstr>, String), ToolError> {
    // Build a SymbolManager with Spotlight so local macOS libs can be found.
    let config = SymbolManagerConfig::new().use_spotlight(true);
    let mut manager = SymbolManager::with_config(config);

    // Teach the manager about this library so it can find its binary on disk.
    let lib_info = build_library_info(lib);
    manager.add_known_library(lib_info.clone());

    // Build the /asm/v1 JSON request.
    let request = build_asm_request(lib, start_rel, size_bytes);

    // Call the JSON API (requires wholesym/api feature).
    let response_json = manager.query_json_api("/asm/v1", &request).await;

    parse_asm_response(&response_json, start_rel)
}

fn build_library_info(lib: &crate::profile::raw::RawLib) -> wholesym::LibraryInfo {
    use std::str::FromStr;
    wholesym::LibraryInfo {
        name: lib.name.clone(),
        debug_name: lib.debug_name.clone(),
        path: lib.path.clone(),
        debug_path: lib.debug_path.clone(),
        debug_id: lib
            .breakpad_id
            .as_deref()
            .and_then(|id| debugid::DebugId::from_breakpad(id).ok()),
        code_id: lib
            .code_id
            .as_deref()
            .and_then(|id| wholesym::CodeId::from_str(id).ok()),
        arch: lib.arch.clone(),
    }
}

fn build_asm_request(lib: &crate::profile::raw::RawLib, start_rel: u32, size_bytes: u32) -> String {
    let mut obj = serde_json::json!({
        "startAddress": format!("0x{start_rel:x}"),
        "size":         format!("0x{size_bytes:x}"),
    });
    if let Some(name) = &lib.name {
        obj["name"] = serde_json::Value::String(name.clone());
    }
    if let Some(debug_name) = &lib.debug_name {
        obj["debugName"] = serde_json::Value::String(debug_name.clone());
    }
    if let Some(debug_id) = &lib.breakpad_id {
        obj["debugId"] = serde_json::Value::String(debug_id.clone());
    }
    if let Some(code_id) = &lib.code_id {
        obj["codeId"] = serde_json::Value::String(code_id.clone());
    }
    obj.to_string()
}

/// Parse the JSON response from samply-api `/asm/v1`.
///
/// The response looks like:
/// ```json
/// {
///   "startAddress": "0x1234",
///   "size": "0x3c",
///   "arch": "aarch64",
///   "syntax": ["ARM"],
///   "instructions": [[0, "stp x29, x30, [sp, #-0x10]!"], [4, "mov x29, sp"], ...]
/// }
/// ```
///
/// Each instruction is `[offset, text, ...]`.  We compute `len` as the
/// difference to the next instruction's offset (last instruction gets the
/// remaining bytes to `size`).
fn parse_asm_response(
    json: &str,
    start_rel: u32,
) -> Result<(Vec<DecodedInstr>, String), ToolError> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| ToolError::Internal {
        message: format!("asm/v1 response parse error: {e}"),
    })?;

    if let Some(err) = v.get("error") {
        return Err(ToolError::Internal {
            message: format!("asm/v1 error: {}", err.as_str().unwrap_or_default()),
        });
    }

    let arch = v
        .get("arch")
        .and_then(|a| a.as_str())
        .unwrap_or("unknown")
        .to_owned();

    let total_size = parse_hex_field(&v, "size").unwrap_or(0);

    let instructions_raw = v
        .get("instructions")
        .and_then(|i| i.as_array())
        .ok_or_else(|| ToolError::Internal {
            message: "asm/v1 response missing 'instructions' array".to_owned(),
        })?;

    let mut raw_pairs: Vec<(u32, String)> = Vec::with_capacity(instructions_raw.len());
    for instr in instructions_raw {
        let arr = instr.as_array().ok_or_else(|| ToolError::Internal {
            message: "asm/v1 instruction is not an array".to_owned(),
        })?;
        let offset = arr
            .first()
            .and_then(|el| el.as_u64())
            .ok_or_else(|| ToolError::Internal {
                message: "asm/v1 instruction missing offset".to_owned(),
            })? as u32;
        // The text is the second element (first syntax).
        let text = arr
            .get(1)
            .and_then(|el| el.as_str())
            .unwrap_or("?")
            .to_owned();
        raw_pairs.push((offset, text));
    }

    // Compute lengths from adjacent offsets.
    let mut decoded: Vec<DecodedInstr> = Vec::with_capacity(raw_pairs.len());
    for (i, &(offset, ref text)) in raw_pairs.iter().enumerate() {
        let next_offset = raw_pairs.get(i + 1).map(|&(o, _)| o).unwrap_or(total_size);
        let len = next_offset.saturating_sub(offset);
        decoded.push(DecodedInstr {
            offset,
            len,
            text: text.clone(),
        });
    }

    // The response startAddress may differ from what we requested (e.g. ARM
    // thumb-bit alignment).  We shift offsets so they are relative to the
    // *requested* start_rel by adjusting with the reported startAddress.
    let reported_start = parse_hex_field(&v, "startAddress").unwrap_or(start_rel);
    if reported_start != start_rel {
        let delta = reported_start.wrapping_sub(start_rel);
        for d in &mut decoded {
            d.offset = d.offset.wrapping_add(delta);
        }
    }

    Ok((decoded, arch))
}

fn parse_hex_field(v: &serde_json::Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(|s| s.as_str())
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
}

// ─── sample attribution ──────────────────────────────────────────────────────

fn attribute_samples(
    decoded: &[DecodedInstr],
    start_rel: u32,
    frame_counts: &HashMap<u32, u64>,
) -> Vec<AsmInstruction> {
    decoded
        .iter()
        .map(|instr| {
            let instr_abs_start = start_rel.wrapping_add(instr.offset);
            let instr_abs_end = instr_abs_start.wrapping_add(instr.len);

            let samples: u64 = frame_counts
                .iter()
                .filter(|&(&addr, _)| {
                    if instr.len == 0 {
                        addr == instr_abs_start
                    } else {
                        addr >= instr_abs_start && addr < instr_abs_end
                    }
                })
                .map(|(_, &count)| count)
                .sum();

            AsmInstruction {
                offset: instr.offset,
                asm: instr.text.clone(),
                samples,
            }
        })
        .collect()
}

// ─── public entry point ──────────────────────────────────────────────────────

pub async fn asm_for_function(profile: &Profile, args: &Args) -> Result<AsmListing, ToolError> {
    let matcher = FunctionMatcher::new(&args.function).map_err(|e| ToolError::Internal {
        message: e.to_string(),
    })?;

    let loc = resolve_function(profile, &matcher, args.module.as_deref()).ok_or_else(|| {
        ToolError::FunctionNotFound {
            function: matcher_to_string(&matcher),
            nearest_matches: nearest_function_names(profile, &matcher),
        }
    })?;

    // Look up the lib.  We search top-level libs first, then sub-process libs.
    let lib = profile
        .lib(loc.lib_idx)
        .ok_or_else(|| ToolError::Internal {
            message: format!("lib index {} out of range", loc.lib_idx),
        })?
        .clone();

    let module_name = lib.name.clone();

    let (decoded, arch) = disassemble(&lib, loc.start_rel, loc.size_bytes).await?;

    let instructions = attribute_samples(&decoded, loc.start_rel, &loc.frame_counts);

    Ok(AsmListing {
        function: args.function.clone(),
        module: module_name,
        start_address: format!("0x{:x}", loc.start_rel),
        size: format!("0x{:x}", loc.size_bytes),
        arch,
        instructions,
    })
}
