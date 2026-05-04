//! Classifier for which raw stream (`samples` vs a named `markers` slice)
//! a per-sample iterator should walk. Lives in the profile module so
//! `Profile::stack_indices` can take it without a back-reference into
//! the query layer; resolution-from-string and "did you mean" suggestions
//! still live in `query::event`.

/// Where a per-sample stack index comes from. Matches the two layouts
/// samply emits: cycles in `samples`, all other hardware events in
/// `markers` keyed by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EventSource {
    /// The default samples track. samply records the first perf event
    /// (typically `cycles`) here; pct columns mean "% of cycles" or
    /// equivalently "% of CPU time".
    #[default]
    Samples,
    /// Markers whose `name[i]` resolves to this string. samply uses this
    /// for secondary perf events (cache-misses, branch-misses,
    /// instructions, etc.).
    Marker(String),
}

impl EventSource {
    /// True iff the source's per-event count multiplied by
    /// `meta.interval` produces a meaningful wall-time-ish duration.
    /// `Samples` is, all marker-backed events are not.
    pub fn is_time_shaped(&self) -> bool {
        matches!(self, EventSource::Samples)
    }

    /// Stable lowercase label for output payloads ("samples" or the
    /// marker name verbatim).
    pub fn label(&self) -> &str {
        match self {
            EventSource::Samples => "samples",
            EventSource::Marker(name) => name.as_str(),
        }
    }
}
