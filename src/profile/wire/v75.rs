//! Shared layout, version 75.
//!
//! Column semantics follow the Firefox Profiler's
//! `docs-developer/CHANGELOG-formats.md`. Nullable columns hold 0 when
//! unset, and a flag bit says whether the value is meaningful.

use serde::Deserialize;

use super::{WireProfile, col, decode_thread, map_string};
use crate::profile::raw::RawProfile;
use crate::profile::tables::SharedTables;

// Frame flags (version 71).
const FRAME_IS_INLINED: u8 = 1 << 0;
const FRAME_HAS_ADDRESS: u8 = 1 << 1;
const FRAME_HAS_NATIVE_SYMBOL: u8 = 1 << 3;
const FRAME_HAS_LINE: u8 = 1 << 4;
const FRAME_HAS_COLUMN: u8 = 1 << 5;

// Func flags (version 75).
const FUNC_IS_JS: u8 = 1 << 0;
const FUNC_RELEVANT_FOR_JS: u8 = 1 << 1;
const FUNC_HAS_RESOURCE: u8 = 1 << 2;
const FUNC_HAS_SOURCE: u8 = 1 << 3;
const FUNC_HAS_LINE: u8 = 1 << 4;
const FUNC_HAS_COLUMN: u8 = 1 << 5;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireShared {
    pub string_array: Vec<String>,
    pub frame_table: FrameTable,
    pub func_table: FuncTable,
    pub stack_table: StackTable,
    pub resource_table: ResourceTable,
    #[serde(default)]
    pub native_symbols: Option<NativeSymbols>,
    #[serde(default)]
    pub sources: Option<Sources>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTable {
    pub length: usize,
    pub flags: Vec<u8>,
    pub func: Vec<usize>,
    pub line: Vec<i64>,
    pub column: Vec<i64>,
    pub address: Vec<u32>,
    /// Version 70 wrote -1 for frames without a library, version 71
    /// and later write 0 and clear `HasAddress`.
    pub lib: Vec<i64>,
    pub native_symbol: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FuncTable {
    pub length: usize,
    pub flags: Vec<u8>,
    pub name: Vec<usize>,
    pub resource: Vec<i64>,
    pub source: Vec<i64>,
    pub line_number: Vec<i64>,
    pub column_number: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTable {
    pub length: usize,
    pub frame: Vec<usize>,
    pub prefix_offset: Vec<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTable {
    pub length: usize,
    pub name: Vec<usize>,
    pub host: Vec<Option<usize>>,
    #[serde(rename = "type")]
    pub type_: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSymbols {
    pub length: usize,
    pub lib_index: Vec<usize>,
    pub address: Vec<i64>,
    pub name: Vec<usize>,
    /// -1 means unknown (version 74).
    pub function_size: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sources {
    pub length: usize,
    /// String index.
    pub filename: Vec<usize>,
}

/// `value` as an index when `bit` is set in `flags`.
fn gated_index(
    flags: u8,
    bit: u8,
    value: i64,
    column: &str,
    row: usize,
) -> Result<Option<usize>, String> {
    if flags & bit == 0 {
        return Ok(None);
    }
    usize::try_from(value)
        .map(Some)
        .map_err(|_| format!("{column}[{row}] = {value} is negative"))
}

/// `value` as a line or column number when `bit` is set in `flags`.
fn gated_u32(
    flags: u8,
    bit: u8,
    value: i64,
    column: &str,
    row: usize,
) -> Result<Option<u32>, String> {
    if flags & bit == 0 {
        return Ok(None);
    }
    u32::try_from(value)
        .map(Some)
        .map_err(|_| format!("{column}[{row}] = {value} does not fit a line number"))
}

pub(super) fn decode(mut w: WireProfile) -> Result<RawProfile, String> {
    let wire = w
        .shared
        .take()
        .ok_or("version 75 profile has no shared section")?;
    if !w.processes.is_empty() {
        return Err("nested processes are not part of the shared layout".to_owned());
    }
    let mut shared = SharedTables {
        libs: w.libs,
        ..Default::default()
    };
    let smap = shared.strings.intern_all(&wire.string_array);

    let mut source_files = Vec::new();
    if let Some(src) = &wire.sources {
        for i in 0..src.length {
            let f = *col(&src.filename, i, "sources.filename")?;
            source_files.push(map_string(&smap, f, "sources.filename", i)?);
        }
    }

    let rt = &wire.resource_table;
    for r in 0..rt.length {
        let name = *col(&rt.name, r, "resourceTable.name")?;
        shared
            .resources
            .name
            .push(map_string(&smap, name, "resourceTable.name", r)?);
        // An out-of-range host string index dangles rather than failing
        // decode. `clear_dangling_optional_refs` nulls it out afterward.
        let host = *col(&rt.host, r, "resourceTable.host")?;
        shared
            .resources
            .host
            .push(host.and_then(|h| map_string(&smap, h, "resourceTable.host", r).ok()));
        shared
            .resources
            .type_
            .push(*col(&rt.type_, r, "resourceTable.type")?);
    }

    if let Some(ns) = &wire.native_symbols {
        for n in 0..ns.length {
            shared
                .native_symbols
                .lib_index
                .push(*col(&ns.lib_index, n, "nativeSymbols.libIndex")?);
            let address = *col(&ns.address, n, "nativeSymbols.address")?;
            shared
                .native_symbols
                .address
                .push(u32::try_from(address).ok());
            let name = *col(&ns.name, n, "nativeSymbols.name")?;
            shared
                .native_symbols
                .name
                .push(map_string(&smap, name, "nativeSymbols.name", n)?);
            let size = *col(&ns.function_size, n, "nativeSymbols.functionSize")?;
            shared
                .native_symbols
                .function_size
                .push(u64::try_from(size).ok());
        }
    }

    let ft = &wire.func_table;
    for f in 0..ft.length {
        let flags = *col(&ft.flags, f, "funcTable.flags")?;
        let name = *col(&ft.name, f, "funcTable.name")?;
        shared
            .funcs
            .name
            .push(map_string(&smap, name, "funcTable.name", f)?);
        shared.funcs.is_js.push(flags & FUNC_IS_JS != 0);
        shared
            .funcs
            .relevant_for_js
            .push(flags & FUNC_RELEVANT_FOR_JS != 0);
        shared.funcs.resource.push(gated_index(
            flags,
            FUNC_HAS_RESOURCE,
            *col(&ft.resource, f, "funcTable.resource")?,
            "funcTable.resource",
            f,
        )?);
        let source = gated_index(
            flags,
            FUNC_HAS_SOURCE,
            *col(&ft.source, f, "funcTable.source")?,
            "funcTable.source",
            f,
        )?;
        // Same as `resourceTable.host` above: a `source` index past the
        // sources table dangles instead of failing decode.
        let file_name = source.and_then(|s| source_files.get(s).copied());
        shared.funcs.file_name.push(file_name);
        shared.funcs.line_number.push(gated_u32(
            flags,
            FUNC_HAS_LINE,
            *col(&ft.line_number, f, "funcTable.lineNumber")?,
            "funcTable.lineNumber",
            f,
        )?);
        shared.funcs.column_number.push(gated_u32(
            flags,
            FUNC_HAS_COLUMN,
            *col(&ft.column_number, f, "funcTable.columnNumber")?,
            "funcTable.columnNumber",
            f,
        )?);
    }

    let fr = &wire.frame_table;
    for i in 0..fr.length {
        let flags = *col(&fr.flags, i, "frameTable.flags")?;
        shared
            .frames
            .func
            .push(*col(&fr.func, i, "frameTable.func")?);
        // `HasAddress` gates both the address and the library, because
        // an address is only meaningful relative to its library.
        let has_address = flags & FRAME_HAS_ADDRESS != 0;
        let address = *col(&fr.address, i, "frameTable.address")?;
        shared.frames.address.push(has_address.then_some(address));
        shared.frames.lib.push(gated_index(
            flags,
            FRAME_HAS_ADDRESS,
            *col(&fr.lib, i, "frameTable.lib")?,
            "frameTable.lib",
            i,
        )?);
        shared.frames.native_symbol.push(gated_index(
            flags,
            FRAME_HAS_NATIVE_SYMBOL,
            *col(&fr.native_symbol, i, "frameTable.nativeSymbol")?,
            "frameTable.nativeSymbol",
            i,
        )?);
        shared.frames.line.push(gated_u32(
            flags,
            FRAME_HAS_LINE,
            *col(&fr.line, i, "frameTable.line")?,
            "frameTable.line",
            i,
        )?);
        shared.frames.column.push(gated_u32(
            flags,
            FRAME_HAS_COLUMN,
            *col(&fr.column, i, "frameTable.column")?,
            "frameTable.column",
            i,
        )?);
        shared.frames.inlined.push(flags & FRAME_IS_INLINED != 0);
    }

    let st = &wire.stack_table;
    for i in 0..st.length {
        shared
            .stacks
            .frame
            .push(*col(&st.frame, i, "stackTable.frame")?);
        // 0 marks a root. Any other offset points back to the parent,
        // which the format guarantees comes first.
        let offset = *col(&st.prefix_offset, i, "stackTable.prefixOffset")?;
        let prefix = match offset {
            0 => None,
            k if k <= i => Some(i - k),
            k => {
                return Err(format!(
                    "stackTable.prefixOffset[{i}] = {k} points before the table start"
                ));
            }
        };
        shared.stacks.prefix.push(prefix);
    }

    let mut threads = Vec::with_capacity(w.threads.len());
    for (i, t) in w.threads.into_iter().enumerate() {
        threads.push(decode_thread(t, &smap, 0).map_err(|e| format!("threads[{i}]: {e}"))?);
    }

    Ok(RawProfile {
        meta: w.meta,
        shared,
        threads,
        processes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use crate::profile::raw::RawProfile;

    /// Frame 0 is a label frame, frame 1 is native with a line, an
    /// inlined flag, and a native symbol. Func 1 has a resource, a
    /// source, and a line number.
    fn profile(prefix_offset: &str) -> String {
        format!(
            r#"{{
            "meta": {{"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 75}},
            "libs": [{{"name": "libx"}}],
            "shared": {{
                "stringArray": ["root", "leaf", "src.rs", "libx", "ev"],
                "frameTable": {{"length": 2, "flags": [4, 31], "func": [0, 1], "category": [0, 0], "subcategory": [0, 0],
                    "line": [0, 12], "column": [0, 0], "address": [0, 4096], "lib": [0, 0], "nativeSymbol": [0, 0],
                    "innerWindowID": [0, 0], "originalLocation": [0, 0]}},
                "funcTable": {{"length": 2, "flags": [0, 29], "name": [0, 1], "resource": [0, 0], "source": [0, 0],
                    "lineNumber": [0, 10], "columnNumber": [0, 0], "originalLocation": [0, 0]}},
                "stackTable": {{"length": 2, "prefixOffset": {prefix_offset}, "frame": [0, 1]}},
                "resourceTable": {{"length": 1, "name": [3], "host": [null], "type": [1]}},
                "nativeSymbols": {{"length": 1, "address": [4000], "functionSize": [-1], "libIndex": [0], "name": [1]}},
                "sources": {{"length": 1, "id": [null], "filename": [2], "startLine": [1], "startColumn": [1], "sourceMapURL": [null], "content": [null]}},
                "sourceLocationTable": {{"length": 0, "source": [], "line": [], "column": []}}
            }},
            "threads": [{{"tid": "7.1", "pid": "7.1", "registerTime": 0.0, "name": "t",
                "samples": {{"length": 2, "weightType": "samples", "stack": [1, null], "weight": [1, 1], "threadCPUDelta": [0, 0], "timeDeltas": [0.5, 1.0]}},
                "markers": {{"length": 2, "category": [0, 0],
                    "data": [{{"type": "Other event", "cause": {{"stack": 1}}}}, {{"type": "Other event"}}],
                    "endTime": [0, 7.0], "name": [4, 4], "phase": [0, 3], "startTime": [0.25, 0]}}}}]
        }}"#
        )
    }

    fn decode(prefix_offset: &str) -> Result<RawProfile, serde_json::Error> {
        serde_json::from_str(&profile(prefix_offset))
    }

    #[test]
    fn prefix_offset_becomes_absolute_prefix() {
        let p = decode("[0, 1]").unwrap();
        assert_eq!(p.shared.stacks.prefix, vec![None, Some(0)]);
    }

    #[test]
    fn prefix_offset_past_row_is_rejected() {
        let err = decode("[0, 2]").unwrap_err().to_string();
        assert!(
            err.contains("stackTable.prefixOffset[1] = 2 points before the table start"),
            "{err}"
        );
    }

    #[test]
    fn frame_flags_gate_columns() {
        let p = decode("[0, 1]").unwrap();
        let f = &p.shared.frames;
        assert_eq!(f.address, vec![None, Some(4096)]);
        assert_eq!(f.lib, vec![None, Some(0)]);
        assert_eq!(f.line, vec![None, Some(12)]);
        assert_eq!(f.column, vec![None, None]);
        assert_eq!(f.native_symbol, vec![None, Some(0)]);
        assert_eq!(f.inlined, vec![false, true]);
    }

    #[test]
    fn func_flags_gate_columns() {
        let p = decode("[0, 1]").unwrap();
        let t = &p.shared;
        assert_eq!(t.funcs.is_js, vec![false, true]);
        assert_eq!(t.funcs.relevant_for_js, vec![false, false]);
        assert_eq!(t.funcs.resource, vec![None, Some(0)]);
        assert_eq!(t.funcs.file_name[0], None);
        assert_eq!(t.strings.get(t.funcs.file_name[1].unwrap()), Some("src.rs"));
        assert_eq!(t.funcs.line_number, vec![None, Some(10)]);
    }

    #[test]
    fn native_symbol_size_sentinel_is_none() {
        let p = decode("[0, 1]").unwrap();
        assert_eq!(p.shared.native_symbols.function_size, vec![None]);
        assert_eq!(p.shared.native_symbols.address, vec![Some(4000)]);
    }

    #[test]
    fn thread_ids_and_markers_decode() {
        let p = decode("[0, 1]").unwrap();
        let t = &p.threads[0];
        assert_eq!(t.tid, 7);
        assert_eq!(t.pid.suffix, Some(1));
        assert_eq!(t.samples.absolute_times(), vec![0.5, 1.5]);
        assert_eq!(t.markers.start_time, vec![Some(0.25), None]);
        assert_eq!(t.markers.end_time, vec![None, Some(7.0)]);
        let ev = p.shared.strings.position("ev").unwrap();
        assert_eq!(t.markers.name, vec![ev, ev]);
    }
}
