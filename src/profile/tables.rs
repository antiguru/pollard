//! Profile-global tables shared by every thread.
//!
//! Both processed-profile layouts decode into these tables: the
//! per-thread layout (versions 49 through 55) by concatenating each
//! thread's tables, the shared layout (version 75) by mapping
//! `profile.shared` column by column. Nullable values are `Option`s,
//! so format sentinels and flag bits never leave the decoders.

#![allow(dead_code)]

use std::collections::HashMap;

use crate::profile::raw::{InlineFrame, RawLib};

/// Deduplicated string table with an interning index.
///
/// Decoders intern every file string, so each distinct string has
/// exactly one index. Marker lookup by name relies on that.
#[derive(Debug, Default, Clone)]
pub struct Strings {
    values: Vec<String>,
    index: HashMap<String, usize>,
}

impl Strings {
    pub fn get(&self, idx: usize) -> Option<&str> {
        self.values.get(idx).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn position(&self, s: &str) -> Option<usize> {
        self.index.get(s).copied()
    }

    pub fn intern(&mut self, s: &str) -> usize {
        if let Some(&idx) = self.index.get(s) {
            return idx;
        }
        let idx = self.values.len();
        self.values.push(s.to_owned());
        self.index.insert(s.to_owned(), idx);
        idx
    }

    /// Intern every entry of a file's string array and return the map
    /// from file index to table index.
    pub fn intern_all(&mut self, file_strings: &[String]) -> Vec<usize> {
        file_strings.iter().map(|s| self.intern(s)).collect()
    }

    /// Replace the string at `idx` in place. Tests use this to rename
    /// functions in a fixture without disturbing other indices.
    ///
    /// Rebuilds the whole index afterwards: swapping two names by two
    /// `replace` calls (rename `values[0]` to what `values[1]` held,
    /// then vice versa) would otherwise leave one of them unindexed,
    /// since a naive update only repoints the entry for the new string
    /// and clears the entry for the old one.
    pub fn replace(&mut self, idx: usize, s: &str) {
        self.values[idx] = s.to_owned();
        self.index.clear();
        for (i, v) in self.values.iter().enumerate() {
            self.index.entry(v.clone()).or_insert(i);
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct FrameTable {
    pub func: Vec<usize>,
    /// Library-relative address, `None` for label frames.
    pub address: Vec<Option<u32>>,
    /// Index into [`SharedTables::libs`].
    pub lib: Vec<Option<usize>>,
    pub line: Vec<Option<u32>>,
    pub column: Vec<Option<u32>>,
    pub native_symbol: Vec<Option<usize>>,
    pub inlined: Vec<bool>,
}

impl FrameTable {
    pub fn len(&self) -> usize {
        self.func.len()
    }

    pub fn is_empty(&self) -> bool {
        self.func.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub struct FuncTable {
    /// String index.
    pub name: Vec<usize>,
    pub is_js: Vec<bool>,
    pub relevant_for_js: Vec<bool>,
    pub resource: Vec<Option<usize>>,
    /// String index.
    pub file_name: Vec<Option<usize>>,
    pub line_number: Vec<Option<u32>>,
    pub column_number: Vec<Option<u32>>,
}

impl FuncTable {
    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub struct StackTable {
    pub frame: Vec<usize>,
    /// Absolute parent stack index, `None` for roots.
    pub prefix: Vec<Option<usize>>,
}

impl StackTable {
    pub fn len(&self) -> usize {
        self.frame.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frame.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub struct ResourceTable {
    /// String index.
    pub name: Vec<usize>,
    /// String index.
    pub host: Vec<Option<usize>>,
    pub type_: Vec<u8>,
}

impl ResourceTable {
    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub struct NativeSymbolTable {
    /// Index into [`SharedTables::libs`].
    pub lib_index: Vec<usize>,
    pub address: Vec<Option<u32>>,
    /// String index.
    pub name: Vec<usize>,
    pub function_size: Vec<Option<u64>>,
}

impl NativeSymbolTable {
    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

/// All tables of one profile. Thread samples and markers index into
/// these.
#[derive(Debug, Default, Clone)]
pub struct SharedTables {
    pub strings: Strings,
    pub frames: FrameTable,
    pub funcs: FuncTable,
    pub stacks: StackTable,
    pub resources: ResourceTable,
    pub native_symbols: NativeSymbolTable,
    /// One list covering the root profile and every sub-process.
    pub libs: Vec<RawLib>,
    /// Per-frame inline-call chain (innermost-first), populated by
    /// [`crate::profile::symbolicate`]. Index parallel to `frames`;
    /// shorter than `frames` until symbolication resizes it. Not part
    /// of the Firefox processed-profile schema, pollard-internal.
    pub inline_chains: Vec<Vec<InlineFrame>>,
}

pub(crate) fn out_of_range(column: &str, row: usize, value: usize, len: usize) -> String {
    format!("{column}[{row}] = {value} is out of range (length {len})")
}

fn check(column: &str, values: &[usize], len: usize) -> Result<(), String> {
    for (row, &v) in values.iter().enumerate() {
        if v >= len {
            return Err(out_of_range(column, row, v, len));
        }
    }
    Ok(())
}

fn check_opt(column: &str, values: &[Option<usize>], len: usize) -> Result<(), String> {
    for (row, v) in values.iter().enumerate() {
        if let Some(v) = *v
            && v >= len
        {
            return Err(out_of_range(column, row, v, len));
        }
    }
    Ok(())
}

impl SharedTables {
    pub fn inline_chain(&self, frame_idx: usize) -> &[InlineFrame] {
        self.inline_chains
            .get(frame_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn set_inline_chain(&mut self, frame_idx: usize, chain: Vec<InlineFrame>) {
        if self.inline_chains.len() < self.frames.len() {
            self.inline_chains.resize_with(self.frames.len(), Vec::new);
        }
        self.inline_chains[frame_idx] = chain;
    }

    /// Null out every optional cross-reference that points past its
    /// target table: `funcs.file_name` and `resources.host` against
    /// `strings`, `funcs.resource` against `resources`, and
    /// `frames.native_symbol` against `native_symbols`.
    ///
    /// These four are the only ones allowed to dangle. Before this
    /// branch, accessors reached them with `.get()` and degraded, so a
    /// file with a stale optional reference still loaded. This keeps
    /// that behavior while `validate_tables` stays strict about every
    /// structural index.
    pub fn clear_dangling_optional_refs(&mut self) {
        let strings = self.strings.len();
        let resources = self.resources.len();
        let native_symbols = self.native_symbols.len();
        for v in &mut self.funcs.file_name {
            if let Some(idx) = *v
                && idx >= strings
            {
                *v = None;
            }
        }
        for v in &mut self.funcs.resource {
            if let Some(idx) = *v
                && idx >= resources
            {
                *v = None;
            }
        }
        for v in &mut self.resources.host {
            if let Some(idx) = *v
                && idx >= strings
            {
                *v = None;
            }
        }
        for v in &mut self.frames.native_symbol {
            if let Some(idx) = *v
                && idx >= native_symbols
            {
                *v = None;
            }
        }
    }

    /// Check every cross-reference between the tables. Decoders build
    /// each column row by row, so all columns of a table have equal
    /// length by construction.
    ///
    /// `funcTable.resource`, `funcTable.fileName`, `resourceTable.host`,
    /// and `frameTable.nativeSymbol` are not checked here: `TryFrom` runs
    /// `clear_dangling_optional_refs` before this validation, which
    /// already nulls out any entry of those four that points out of
    /// range, so they can no longer fire.
    pub fn validate_tables(&self) -> Result<(), String> {
        let strings = self.strings.len();
        check("frameTable.func", &self.frames.func, self.funcs.len())?;
        check_opt("frameTable.lib", &self.frames.lib, self.libs.len())?;
        check("funcTable.name", &self.funcs.name, strings)?;
        check("stackTable.frame", &self.stacks.frame, self.frames.len())?;
        check_opt("stackTable.prefix", &self.stacks.prefix, self.stacks.len())?;
        check("resourceTable.name", &self.resources.name, strings)?;
        check(
            "nativeSymbols.libIndex",
            &self.native_symbols.lib_index,
            self.libs.len(),
        )?;
        check("nativeSymbols.name", &self.native_symbols.name, strings)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::InlineFrame;

    #[test]
    fn intern_all_deduplicates() {
        let mut s = Strings::default();
        let map = s.intern_all(&["a".into(), "b".into(), "a".into()]);
        assert_eq!(map, vec![0, 1, 0]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.position("a"), Some(0));
        let map2 = s.intern_all(&["b".into(), "c".into()]);
        assert_eq!(map2, vec![1, 2]);
    }

    #[test]
    fn replace_updates_index() {
        let mut s = Strings::default();
        s.intern("old");
        s.replace(0, "new");
        assert_eq!(s.get(0), Some("new"));
        assert_eq!(s.position("new"), Some(0));
        assert_eq!(s.position("old"), None);
    }

    #[test]
    fn replace_swap_keeps_both_indexed() {
        let mut s = Strings::default();
        s.intern("hot");
        s.intern("cold");
        s.replace(0, "cold");
        s.replace(1, "hot");
        assert_eq!(s.position("cold"), Some(0));
        assert_eq!(s.position("hot"), Some(1));
    }

    #[test]
    fn set_inline_chain_grows_to_frame_count() {
        let mut t = SharedTables::default();
        t.frames.func = vec![0, 0, 0];
        t.set_inline_chain(
            2,
            vec![InlineFrame {
                function: "inner".into(),
                file: None,
                line: None,
            }],
        );
        assert_eq!(t.inline_chains.len(), 3);
        assert!(t.inline_chain(0).is_empty());
        assert_eq!(t.inline_chain(2)[0].function, "inner");
        assert!(t.inline_chain(99).is_empty());
    }

    fn one_frame_tables() -> SharedTables {
        let mut t = SharedTables::default();
        t.strings.intern("f");
        t.funcs = FuncTable {
            name: vec![0],
            is_js: vec![false],
            relevant_for_js: vec![false],
            resource: vec![None],
            file_name: vec![None],
            line_number: vec![None],
            column_number: vec![None],
        };
        t.frames = FrameTable {
            func: vec![0],
            address: vec![None],
            lib: vec![None],
            line: vec![None],
            column: vec![None],
            native_symbol: vec![None],
            inlined: vec![false],
        };
        t.stacks = StackTable {
            frame: vec![0],
            prefix: vec![None],
        };
        t
    }

    #[test]
    fn validate_accepts_consistent_tables() {
        assert_eq!(one_frame_tables().validate_tables(), Ok(()));
    }

    #[test]
    fn validate_rejects_out_of_range_func() {
        let mut t = one_frame_tables();
        t.frames.func[0] = 5;
        let err = t.validate_tables().unwrap_err();
        assert_eq!(err, "frameTable.func[0] = 5 is out of range (length 1)");
    }

    #[test]
    fn validate_rejects_out_of_range_prefix() {
        let mut t = one_frame_tables();
        t.stacks.prefix[0] = Some(3);
        let err = t.validate_tables().unwrap_err();
        assert_eq!(err, "stackTable.prefix[0] = 3 is out of range (length 1)");
    }

    #[test]
    fn validate_rejects_missing_lib() {
        let mut t = one_frame_tables();
        t.frames.lib[0] = Some(0);
        let err = t.validate_tables().unwrap_err();
        assert_eq!(err, "frameTable.lib[0] = 0 is out of range (length 0)");
    }

    #[test]
    fn clear_dangling_optional_refs_keeps_valid_and_clears_invalid() {
        let mut t = SharedTables::default();
        t.strings.intern("s0");
        t.strings.intern("s1");
        t.resources.name = vec![0, 0];
        t.resources.host = vec![Some(0), Some(5)];
        t.resources.type_ = vec![0, 0];
        t.native_symbols.lib_index = vec![0];
        t.native_symbols.address = vec![None];
        t.native_symbols.name = vec![0];
        t.native_symbols.function_size = vec![None];
        t.funcs = FuncTable {
            name: vec![0, 0],
            is_js: vec![false, false],
            relevant_for_js: vec![false, false],
            resource: vec![Some(1), Some(9)],
            file_name: vec![Some(1), Some(9)],
            line_number: vec![None, None],
            column_number: vec![None, None],
        };
        t.frames = FrameTable {
            func: vec![0, 0],
            address: vec![None, None],
            lib: vec![None, None],
            line: vec![None, None],
            column: vec![None, None],
            native_symbol: vec![Some(0), Some(4)],
            inlined: vec![false, false],
        };

        t.clear_dangling_optional_refs();

        assert_eq!(t.funcs.file_name, vec![Some(1), None]);
        assert_eq!(t.funcs.resource, vec![Some(1), None]);
        assert_eq!(t.resources.host, vec![Some(0), None]);
        assert_eq!(t.frames.native_symbol, vec![Some(0), None]);
    }
}
