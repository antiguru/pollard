//! Per-thread layout (versions 49 through 55).
//!
//! Each thread carries its own string array and tables. The decoder
//! concatenates them into [`SharedTables`], offsetting every
//! cross-reference, and merges nested processes' `libs` into one list.

use serde::Deserialize;

use super::{WireProfile, WireThread, col, decode_thread, map_string};
use crate::profile::raw::{RawProcess, RawProfile, RawThread};
use crate::profile::tables::SharedTables;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyFrameTable {
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
pub struct LegacyFuncTable {
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
pub struct LegacyStackTable {
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
pub struct LegacyResourceTable {
    pub length: usize,
    pub lib: Vec<Option<usize>>,
    pub name: Vec<usize>, // string-array index
    pub host: Vec<Option<usize>>,
    #[serde(rename = "type")]
    pub type_: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyNativeSymbols {
    pub length: usize,
    pub lib_index: Vec<usize>,
    pub address: Vec<i64>,
    pub name: Vec<usize>, // string-array index
    pub function_size: Vec<Option<u64>>,
}

pub(super) fn decode(w: WireProfile) -> Result<RawProfile, String> {
    if w.shared.is_some() {
        return Err("per-thread layout profile has a shared section".to_owned());
    }
    let mut shared = SharedTables {
        libs: w.libs,
        ..Default::default()
    };
    let mut threads = Vec::with_capacity(w.threads.len());
    for (i, t) in w.threads.into_iter().enumerate() {
        threads.push(append_thread(&mut shared, t, 0).map_err(|e| format!("threads[{i}]: {e}"))?);
    }
    let mut processes = Vec::with_capacity(w.processes.len());
    for (pi, p) in w.processes.into_iter().enumerate() {
        // Each process's resource tables index its own `libs`, so its
        // libraries land after the ones merged so far.
        let lib_base = shared.libs.len();
        shared.libs.extend(p.libs);
        let mut process_threads = Vec::with_capacity(p.threads.len());
        for (i, t) in p.threads.into_iter().enumerate() {
            process_threads.push(
                append_thread(&mut shared, t, lib_base)
                    .map_err(|e| format!("processes[{pi}].threads[{i}]: {e}"))?,
            );
        }
        processes.push(RawProcess {
            threads: process_threads,
        });
    }
    Ok(RawProfile {
        meta: w.meta,
        shared,
        threads,
        processes,
    })
}

fn append_thread(
    shared: &mut SharedTables,
    mut t: WireThread,
    lib_base: usize,
) -> Result<RawThread, String> {
    let strings = t.string_array.take().ok_or("missing stringArray")?;
    let frames = t.frame_table.take().ok_or("missing frameTable")?;
    let funcs = t.func_table.take().ok_or("missing funcTable")?;
    let stacks = t.stack_table.take().ok_or("missing stackTable")?;
    let resources = t.resource_table.take().ok_or("missing resourceTable")?;
    let native_symbols = t.native_symbols.take();

    let smap = shared.strings.intern_all(&strings);
    let func_base = shared.funcs.len();
    let frame_base = shared.frames.len();
    let stack_base = shared.stacks.len();
    let resource_base = shared.resources.len();
    let native_base = shared.native_symbols.len();

    // Resource libs index this thread's process `libs`; keep them local
    // so frames can look them up before the offsets apply.
    let mut resource_libs = Vec::with_capacity(resources.length);
    for r in 0..resources.length {
        let name = *col(&resources.name, r, "resourceTable.name")?;
        shared
            .resources
            .name
            .push(map_string(&smap, name, "resourceTable.name", r)?);
        // An out-of-range host string index dangles rather than failing
        // decode. `clear_dangling_optional_refs` treats it the same as
        // one that lands out of range only after threads are merged.
        let host = *col(&resources.host, r, "resourceTable.host")?;
        shared
            .resources
            .host
            .push(host.and_then(|h| map_string(&smap, h, "resourceTable.host", r).ok()));
        shared
            .resources
            .type_
            .push(*col(&resources.type_, r, "resourceTable.type")?);
        resource_libs.push(col(&resources.lib, r, "resourceTable.lib")?.map(|l| l + lib_base));
    }

    for f in 0..funcs.length {
        let name = *col(&funcs.name, f, "funcTable.name")?;
        shared
            .funcs
            .name
            .push(map_string(&smap, name, "funcTable.name", f)?);
        shared
            .funcs
            .is_js
            .push(*col(&funcs.is_js, f, "funcTable.isJS")?);
        shared.funcs.relevant_for_js.push(*col(
            &funcs.relevant_for_js,
            f,
            "funcTable.relevantForJS",
        )?);
        let resource = *col(&funcs.resource, f, "funcTable.resource")?;
        shared
            .funcs
            .resource
            .push(usize::try_from(resource).ok().map(|r| r + resource_base));
        // Same as `resourceTable.host` above: dangle instead of failing.
        let file = *col(&funcs.file_name, f, "funcTable.fileName")?;
        shared
            .funcs
            .file_name
            .push(file.and_then(|s| map_string(&smap, s, "funcTable.fileName", f).ok()));
        shared
            .funcs
            .line_number
            .push(funcs.line_number.get(f).copied().flatten());
        shared
            .funcs
            .column_number
            .push(funcs.column_number.get(f).copied().flatten());
    }

    if let Some(ns) = native_symbols {
        for n in 0..ns.length {
            let lib = *col(&ns.lib_index, n, "nativeSymbols.libIndex")?;
            shared.native_symbols.lib_index.push(lib + lib_base);
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
            shared
                .native_symbols
                .function_size
                .push(ns.function_size.get(n).copied().flatten());
        }
    }

    for fr in 0..frames.length {
        let func = *col(&frames.func, fr, "frameTable.func")?;
        shared.frames.func.push(func + func_base);
        let address = *col(&frames.address, fr, "frameTable.address")?;
        shared.frames.address.push(u32::try_from(address).ok());
        // The per-thread layout reaches a frame's library through
        // func, then resource, then resource.lib.
        let lib = funcs
            .resource
            .get(func)
            .and_then(|&r| usize::try_from(r).ok())
            .and_then(|r| resource_libs.get(r).copied().flatten());
        shared.frames.lib.push(lib);
        shared
            .frames
            .line
            .push(frames.line.get(fr).copied().flatten());
        shared
            .frames
            .column
            .push(frames.column.get(fr).copied().flatten());
        shared.frames.native_symbol.push(
            frames
                .native_symbol
                .get(fr)
                .copied()
                .flatten()
                .map(|n| n + native_base),
        );
        shared.frames.inlined.push(false);
    }

    for s in 0..stacks.length {
        let frame = *col(&stacks.frame, s, "stackTable.frame")?;
        shared.stacks.frame.push(frame + frame_base);
        let prefix = *col(&stacks.prefix, s, "stackTable.prefix")?;
        shared.stacks.prefix.push(prefix.map(|p| p + stack_base));
    }

    decode_thread(t, &smap, stack_base)
}

#[cfg(test)]
mod tests {
    use super::super::WireProfile;
    use super::decode;

    /// Two threads whose tables both start at index 0, so every index
    /// in the second thread must move.
    const TWO_THREADS: &str = r#"{
        "meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 49},
        "libs": [{"name": "root"}],
        "threads": [
            {"tid": 1, "pid": 1, "registerTime": 0.0,
             "stringArray": ["a", "ev"],
             "frameTable": {"length": 1, "address": [16], "func": [0], "line": [null], "column": [null], "category": [0], "subcategory": [0], "nativeSymbol": [null]},
             "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [0], "fileName": [null], "lineNumber": [null], "columnNumber": [null]},
             "stackTable": {"length": 1, "frame": [0], "prefix": [null]},
             "resourceTable": {"length": 1, "lib": [0], "name": [0], "host": [null], "type": [1]},
             "samples": {"length": 1, "stack": [0], "time": [0.0]},
             "markers": {"length": 1, "data": [{"type": "Other event", "cause": {"stack": 0}}], "name": [1], "startTime": [0.5], "endTime": [null], "phase": [0], "category": [0]}},
            {"tid": 2, "pid": 1, "registerTime": 0.0,
             "stringArray": ["b", "a"],
             "frameTable": {"length": 2, "address": [-1, 32], "func": [0, 1], "line": [null, 7], "column": [null, null], "category": [0, 0], "subcategory": [0, 0], "nativeSymbol": [null, null]},
             "funcTable": {"length": 2, "name": [0, 1], "isJS": [false, false], "relevantForJS": [false, false], "resource": [-1, 0], "fileName": [null, 0], "lineNumber": [null, null], "columnNumber": [null, null]},
             "stackTable": {"length": 2, "frame": [0, 1], "prefix": [null, 0]},
             "resourceTable": {"length": 1, "lib": [0], "name": [1], "host": [null], "type": [1]},
             "samples": {"length": 2, "stack": [1, null], "time": [0.0, 1.0]}}
        ]
    }"#;

    fn decode_str(json: &str) -> crate::profile::raw::RawProfile {
        let w: WireProfile = serde_json::from_str(json).unwrap();
        decode(w).unwrap()
    }

    #[test]
    fn second_thread_indices_are_offset() {
        let p = decode_str(TWO_THREADS);
        let t = &p.shared;
        assert_eq!(t.frames.len(), 3);
        assert_eq!(t.funcs.len(), 3);
        assert_eq!(t.stacks.len(), 3);
        // Thread 2's stack 1 is shared stack 2, whose prefix is thread 2's stack 0 (shared 1).
        assert_eq!(p.threads[1].samples.stack, vec![Some(2), None]);
        assert_eq!(t.stacks.prefix[2], Some(1));
        assert_eq!(t.stacks.frame[2], 2);
        assert_eq!(t.frames.func[2], 2);
        assert_eq!(t.frames.address[1], None);
        assert_eq!(t.frames.address[2], Some(32));
        assert_eq!(t.frames.line[2], Some(7));
    }

    #[test]
    fn duplicate_strings_merge() {
        let p = decode_str(TWO_THREADS);
        let t = &p.shared;
        // "a" appears in both threads and maps to one entry.
        assert_eq!(t.strings.len(), 3);
        assert_eq!(t.funcs.name[0], t.funcs.name[2]);
        assert_eq!(t.funcs.file_name[2], Some(t.strings.position("b").unwrap()));
        assert_eq!(
            p.threads[0].markers.name,
            vec![t.strings.position("ev").unwrap()]
        );
    }

    #[test]
    fn frame_lib_comes_from_resource() {
        let p = decode_str(TWO_THREADS);
        let t = &p.shared;
        assert_eq!(t.frames.lib, vec![Some(0), None, Some(0)]);
        assert_eq!(t.funcs.resource, vec![Some(0), None, Some(1)]);
    }

    #[test]
    fn marker_times_follow_phase() {
        let p = decode_str(TWO_THREADS);
        let m = &p.threads[0].markers;
        assert_eq!(m.start_time, vec![Some(0.5)]);
        assert_eq!(m.end_time, vec![None]);
        let data = m.data[0].as_ref().unwrap();
        assert_eq!(data.type_.as_deref(), Some("Other event"));
        assert_eq!(data.cause.as_ref().unwrap().stack, 0);
    }

    #[test]
    fn sub_process_libs_are_remapped() {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0},
            "libs": [{"name": "root"}],
            "threads": [],
            "processes": [{
                "libs": [{"name": "child"}],
                "threads": [
                    {"tid": 3, "pid": 2, "registerTime": 0.0,
                     "stringArray": ["c"],
                     "frameTable": {"length": 1, "address": [8], "func": [0], "line": [null], "column": [null], "category": [0], "subcategory": [0], "nativeSymbol": [0]},
                     "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [0], "fileName": [null], "lineNumber": [null], "columnNumber": [null]},
                     "stackTable": {"length": 1, "frame": [0], "prefix": [null]},
                     "resourceTable": {"length": 1, "lib": [0], "name": [0], "host": [null], "type": [1]},
                     "nativeSymbols": {"length": 1, "libIndex": [0], "address": [0], "name": [0], "functionSize": [null]},
                     "samples": {"length": 1, "stack": [0], "time": [0.0]}}
                ]
            }]
        }"#;
        let p = decode_str(json);
        let t = &p.shared;
        assert_eq!(t.libs.len(), 2);
        assert_eq!(t.libs[1].name.as_deref(), Some("child"));
        assert_eq!(t.frames.lib, vec![Some(1)]);
        assert_eq!(t.native_symbols.lib_index, vec![1]);
        assert_eq!(p.processes[0].threads[0].tid, 3);
    }

    #[test]
    fn dangling_file_name_loads_as_none() {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 49},
            "libs": [{"name": "root"}],
            "threads": [
                {"tid": 1, "pid": 1, "registerTime": 0.0,
                 "stringArray": ["a"],
                 "frameTable": {"length": 1, "address": [16], "func": [0], "line": [null], "column": [null], "category": [0], "subcategory": [0], "nativeSymbol": [null]},
                 "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [-1], "fileName": [99], "lineNumber": [null], "columnNumber": [null]},
                 "stackTable": {"length": 1, "frame": [0], "prefix": [null]},
                 "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                 "samples": {"length": 1, "stack": [0], "time": [0.0]}}
            ]
        }"#;
        let w: WireProfile = serde_json::from_str(json).unwrap();
        let p: crate::profile::raw::RawProfile = w.try_into().unwrap();
        assert_eq!(p.shared.funcs.file_name, vec![None]);
    }

    #[test]
    fn missing_string_array_is_reported() {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0},
            "threads": [{"tid": 1, "pid": 1, "registerTime": 0.0,
                         "samples": {"length": 0, "stack": []}}]
        }"#;
        let w: WireProfile = serde_json::from_str(json).unwrap();
        let err = decode(w).unwrap_err();
        assert_eq!(err, "threads[0]: missing stringArray");
    }
}
