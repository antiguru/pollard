//! The `Perf events` section that samply writes into `meta.extra`.
//!
//! samply lists one entry per perf event attribute, in attribute order,
//! then one `Sample weight` entry. The first event is the samples track.
//! Values follow this grammar, with `N` a decimal integer:
//!
//! ```text
//! event value  = "frequency " N " Hz" | "period " N
//! weight value = "period" | "1"
//! ```
//!
//! Values outside the grammar are ignored.

use crate::profile::raw::RawExtraSection;

/// Label of the section samply writes.
pub const SECTION_LABEL: &str = "Perf events";
/// Label of the entry that says how samples are weighted.
pub const SAMPLE_WEIGHT_LABEL: &str = "Sample weight";

/// Whether a period value counts as a period rather than being absent:
/// zero, negative, and non-finite values are absent. Shared so `fixed_period`
/// below and Task 10's marker weights and `is_weighted` agree on the same
/// rule.
pub(crate) fn is_positive_period(period: f64) -> bool {
    period.is_finite() && period > 0.0
}

/// How perf sampled one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
    /// `perf record -F N`: perf adjusts the period to reach N samples per second.
    Frequency(u64),
    /// `perf record -c N`: one sample every N events.
    Period(u64),
}

impl Sampling {
    /// Parse an event value, or `None` when it does not match the grammar.
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(rest) = value.strip_prefix("frequency ") {
            return parse_decimal(rest.strip_suffix(" Hz")?).map(Sampling::Frequency);
        }
        parse_decimal(value.strip_prefix("period ")?).map(Sampling::Period)
    }

    /// The event value in the grammar's form, e.g. `frequency 999 Hz`.
    pub fn describe(&self) -> String {
        match self {
            Sampling::Frequency(hz) => format!("frequency {hz} Hz"),
            Sampling::Period(n) => format!("period {n}"),
        }
    }
}

/// A non-empty run of ASCII digits. `u64::from_str` alone would accept `+5`.
fn parse_decimal(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// One perf event of the section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerfEvent {
    /// The event name as samply recorded it, e.g. `cycles:u`.
    pub label: String,
    /// `None` when the value does not match the grammar.
    pub sampling: Option<Sampling>,
}

/// The parsed `Perf events` section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerfEvents {
    /// Events in attribute order. The first is the samples track.
    pub events: Vec<PerfEvent>,
    /// True when `Sample weight` reads `period`.
    pub weight_by_period: bool,
}

impl PerfEvents {
    /// Parse the first `Perf events` section, or `None` when there is none.
    pub fn from_extra(sections: &[RawExtraSection]) -> Option<Self> {
        let section = sections.iter().find(|s| s.label == SECTION_LABEL)?;
        let mut out = PerfEvents::default();
        for entry in &section.entries {
            let value = entry.value.as_str();
            if entry.label == SAMPLE_WEIGHT_LABEL {
                out.weight_by_period = value == Some("period");
                continue;
            }
            out.events.push(PerfEvent {
                label: entry.label.clone(),
                sampling: value.and_then(Sampling::parse),
            });
        }
        Some(out)
    }

    /// The event behind the samples track.
    pub fn main_event(&self) -> Option<&PerfEvent> {
        self.events.first()
    }

    /// The first event with this label. Markers of equally named events
    /// are indistinguishable, so later duplicates never match.
    pub fn event(&self, label: &str) -> Option<&PerfEvent> {
        self.events.iter().find(|e| e.label == label)
    }

    /// The fixed period of the event with this label, when it was
    /// recorded with `perf record -c`. `None` for a `period 0` entry, since
    /// that carries no usable weight.
    pub fn fixed_period(&self, label: &str) -> Option<u64> {
        match self.event(label)?.sampling? {
            Sampling::Period(n) if is_positive_period(n as f64) => Some(n),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::{RawExtraEntry, RawExtraSection};

    fn section(label: &str, entries: &[(&str, serde_json::Value)]) -> Vec<RawExtraSection> {
        vec![RawExtraSection {
            label: label.to_owned(),
            entries: entries
                .iter()
                .map(|(l, v)| RawExtraEntry {
                    label: (*l).to_owned(),
                    format: "string".to_owned(),
                    value: v.clone(),
                })
                .collect(),
        }]
    }

    #[test]
    fn parses_events_and_weight_mode() {
        let extra = section(
            "Perf events",
            &[
                ("cycles:u", "frequency 999 Hz".into()),
                ("cache-misses", "period 10000".into()),
                ("Sample weight", "period".into()),
            ],
        );
        let p = PerfEvents::from_extra(&extra).unwrap();
        assert!(p.weight_by_period);
        assert_eq!(p.events.len(), 2);
        assert_eq!(p.main_event().unwrap().label, "cycles:u");
        assert_eq!(
            p.main_event().unwrap().sampling,
            Some(Sampling::Frequency(999))
        );
        assert_eq!(p.fixed_period("cache-misses"), Some(10_000));
        assert_eq!(p.fixed_period("cycles:u"), None);
        assert_eq!(p.fixed_period("missing"), None);
    }

    #[test]
    fn sample_weight_one_is_not_period_weighting() {
        let extra = section(
            "Perf events",
            &[("cycles", "period 5".into()), ("Sample weight", "1".into())],
        );
        assert!(!PerfEvents::from_extra(&extra).unwrap().weight_by_period);
    }

    #[test]
    fn invalid_values_are_ignored() {
        for bad in [
            serde_json::json!("frequency x Hz"),
            serde_json::json!("frequency 99"),
            serde_json::json!("period -5"),
            serde_json::json!("period +5"),
            serde_json::json!("period 1 "),
            serde_json::json!("Period 5"),
            serde_json::json!("period "),
            serde_json::json!("no sampling"),
            serde_json::json!(7),
        ] {
            let extra = section(
                "Perf events",
                &[("ev", bad.clone()), ("Sample weight", "2".into())],
            );
            let p = PerfEvents::from_extra(&extra).unwrap();
            assert_eq!(p.events[0].label, "ev", "{bad}");
            assert_eq!(p.events[0].sampling, None, "{bad}");
            assert!(!p.weight_by_period, "{bad}");
        }
    }

    #[test]
    fn first_entry_with_a_label_wins() {
        let extra = section(
            "Perf events",
            &[("cycles", "period 5".into()), ("cycles", "period 9".into())],
        );
        assert_eq!(
            PerfEvents::from_extra(&extra)
                .unwrap()
                .fixed_period("cycles"),
            Some(5)
        );
    }

    #[test]
    fn other_sections_are_not_perf_events() {
        let extra = section("Something else", &[("cycles", "period 5".into())]);
        assert!(PerfEvents::from_extra(&extra).is_none());
        assert!(PerfEvents::from_extra(&[]).is_none());
    }

    #[test]
    fn describe_round_trips_the_grammar() {
        for s in ["frequency 999 Hz", "period 10000", "period 0"] {
            assert_eq!(Sampling::parse(s).unwrap().describe(), s);
        }
    }
}
