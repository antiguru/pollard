//! Wire formats of the Firefox processed profile and the version gate
//! that picks a decoder.
//!
//! [`WireProfile`] deserializes both layouts in one serde pass: the
//! per-thread tables live on [`WireThread`], the shared tables on
//! [`WireProfile::shared`]. The decoders turn either into
//! [`crate::profile::tables::SharedTables`] plus threads that keep
//! only samples and markers.

#![allow(dead_code)]

mod legacy;
mod v75;

use serde::Deserialize;

use crate::profile::raw::{
    Pid, RawLib, RawMarkerData, RawMarkerTable, RawMeta, RawProfile, RawSampleTable, RawThread,
    deserialize_id_as_u64,
};
use crate::profile::tables::out_of_range;

/// Versions the decoders understand. A missing version counts as the
/// per-thread layout, because the hand-written test fixtures carry none.
pub(crate) fn is_supported_version(v: u32) -> bool {
    matches!(v, 49..=55 | 75)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireProfile {
    pub meta: RawMeta,
    #[serde(default)]
    pub libs: Vec<RawLib>,
    /// Shared layout only.
    #[serde(default)]
    pub shared: Option<v75::WireShared>,
    #[serde(default)]
    pub threads: Vec<WireThread>,
    #[serde(default)]
    pub processes: Vec<WireProcess>,
}

/// A nested process of the per-thread layout. It carries its own
/// `libs` list, which its resource tables index into.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireProcess {
    #[serde(default)]
    pub libs: Vec<RawLib>,
    #[serde(default)]
    pub threads: Vec<WireThread>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireThread {
    #[serde(deserialize_with = "deserialize_id_as_u64")]
    pub tid: u64,
    pub pid: Pid,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub process_name: Option<String>,
    pub register_time: f64,
    pub samples: RawSampleTable,
    /// `default` so older fixtures without a markers field still parse.
    #[serde(default)]
    pub markers: WireMarkerTable,
    // Per-thread layout only. The shared layout keeps these in
    // `profile.shared`.
    #[serde(default)]
    pub string_array: Option<Vec<String>>,
    #[serde(default)]
    pub frame_table: Option<legacy::LegacyFrameTable>,
    #[serde(default)]
    pub func_table: Option<legacy::LegacyFuncTable>,
    #[serde(default)]
    pub stack_table: Option<legacy::LegacyStackTable>,
    #[serde(default)]
    pub resource_table: Option<legacy::LegacyResourceTable>,
    #[serde(default)]
    pub native_symbols: Option<legacy::LegacyNativeSymbols>,
}

/// Marker columns as stored. Times may be `null` where the phase makes
/// them meaningless (format version 68 and later).
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WireMarkerTable {
    pub length: usize,
    pub data: Vec<Option<RawMarkerData>>,
    pub name: Vec<usize>,
    pub start_time: Vec<Option<f64>>,
    pub end_time: Vec<Option<f64>>,
    pub phase: Vec<u8>,
    pub category: Vec<usize>,
}

impl TryFrom<WireProfile> for RawProfile {
    type Error = String;

    fn try_from(w: WireProfile) -> Result<Self, String> {
        let profile = match w.meta.preprocessed_profile_version {
            None => legacy::decode(w)?,
            Some(75) => v75::decode(w)?,
            Some(v) if is_supported_version(v) => legacy::decode(w)?,
            Some(v) => return Err(format!("unsupported processed-profile version {v}")),
        };
        validate(&profile)?;
        Ok(profile)
    }
}

/// Check every index a thread holds, after the table-level checks.
fn validate(p: &RawProfile) -> Result<(), String> {
    p.shared.validate_tables()?;
    let stacks = p.shared.stacks.len();
    let strings = p.shared.strings.len();
    let threads = p
        .threads
        .iter()
        .chain(p.processes.iter().flat_map(|pr| pr.threads.iter()));
    for t in threads {
        for (row, s) in t.samples.stack.iter().enumerate() {
            if let Some(s) = *s
                && s >= stacks
            {
                return Err(out_of_range("samples.stack", row, s, stacks));
            }
        }
        for (row, &n) in t.markers.name.iter().enumerate() {
            if n >= strings {
                return Err(out_of_range("markers.name", row, n, strings));
            }
        }
        for (row, d) in t.markers.data.iter().enumerate() {
            if let Some(c) = d.as_ref().and_then(|d| d.cause.as_ref())
                && c.stack >= stacks
            {
                return Err(out_of_range(
                    "markers.data.cause.stack",
                    row,
                    c.stack,
                    stacks,
                ));
            }
        }
    }
    Ok(())
}

/// Row `row` of `column`, or an error naming the column.
pub(super) fn col<'a, T>(column: &'a [T], row: usize, name: &str) -> Result<&'a T, String> {
    column
        .get(row)
        .ok_or_else(|| format!("{name} has no row {row}"))
}

/// Translate a file string index through the map returned by
/// [`crate::profile::tables::Strings::intern_all`].
pub(super) fn map_string(
    map: &[usize],
    idx: usize,
    column: &str,
    row: usize,
) -> Result<usize, String> {
    map.get(idx)
        .copied()
        .ok_or_else(|| out_of_range(column, row, idx, map.len()))
}

/// Drop the marker time the phase marks as meaningless: instant (0)
/// and interval-start (2) markers have no end, interval-end (3)
/// markers have no start.
pub(super) fn phase_times(
    phase: u8,
    start: Option<f64>,
    end: Option<f64>,
) -> (Option<f64>, Option<f64>) {
    match phase {
        0 | 2 => (start, None),
        3 => (None, end),
        _ => (start, end),
    }
}

/// Build a decoded thread. `string_map` translates the file's string
/// indices and `stack_base` moves the thread's stack indices into the
/// shared stack table.
pub(super) fn decode_thread(
    t: WireThread,
    string_map: &[usize],
    stack_base: usize,
) -> Result<RawThread, String> {
    let mut samples = t.samples;
    for stack in samples.stack.iter_mut().flatten() {
        *stack += stack_base;
    }

    let m = t.markers;
    let mut markers = RawMarkerTable {
        length: m.length,
        category: m.category,
        phase: m.phase,
        ..Default::default()
    };
    for (row, &name) in m.name.iter().enumerate() {
        markers
            .name
            .push(map_string(string_map, name, "markers.name", row)?);
    }
    for (row, data) in m.data.into_iter().enumerate() {
        markers.data.push(data.map(|mut d| {
            if let Some(cause) = d.cause.as_mut() {
                cause.stack += stack_base;
            }
            d
        }));
        let phase = markers.phase.get(row).copied().unwrap_or(0);
        let start = m.start_time.get(row).copied().flatten();
        let end = m.end_time.get(row).copied().flatten();
        let (start, end) = phase_times(phase, start, end);
        markers.start_time.push(start);
        markers.end_time.push(end);
    }

    Ok(RawThread {
        tid: t.tid,
        pid: t.pid,
        name: t.name,
        process_name: t.process_name,
        register_time: t.register_time,
        samples,
        markers,
    })
}
