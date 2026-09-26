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

/// A marker period as an event count, when it counts as a period rather
/// than being absent: values whose [`event_count`] is `None` or 0 are
/// absent. This covers zero, negative, non-finite, and values in (0, 1).
pub(crate) fn marker_period(period: f64) -> Option<u64> {
    event_count(period).filter(|&n| n > 0)
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
            // Same rule as `marker_period`: zero counts as absent.
            // `n` is a `u64`, so it can't be negative or non-finite.
            Sampling::Period(n) if n > 0 => Some(n),
            _ => None,
        }
    }
}

/// A period or weight as an event count, when it is a finite non-negative
/// number. Fractions are truncated.
pub fn event_count(v: f64) -> Option<u64> {
    // No lossless f64 to u64 conversion exists. `as` truncates and
    // saturates above u64::MAX.
    (v.is_finite() && v >= 0.0).then_some(v as u64)
}

/// Weight of sample `i`: its `samples.weight` entry when the profile is
/// weighted by period, else 1. A negative or non-finite entry weighs 0,
/// and a missing entry weighs 1.
pub fn sample_weight(weights: Option<&[f64]>, i: usize, weight_by_period: bool) -> u64 {
    if !weight_by_period {
        return 1;
    }
    match weights.and_then(|w| w.get(i)) {
        Some(&w) => event_count(w).unwrap_or(0),
        None => 1,
    }
}

/// Weight of a marker: its `period` when [`marker_period`] counts it, else
/// its event's fixed period, else 1.
pub fn marker_weight(period: Option<f64>, fixed_period: Option<u64>) -> u64 {
    period.and_then(marker_period).or(fixed_period).unwrap_or(1)
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
    fn fixed_period_is_none_for_a_zero_period() {
        let extra = section("Perf events", &[("ev", "period 0".into())]);
        let p = PerfEvents::from_extra(&extra).unwrap();
        assert_eq!(p.fixed_period("ev"), None);
    }

    #[test]
    fn fixed_period_is_some_for_a_positive_period() {
        let extra = section("Perf events", &[("ev", "period 5".into())]);
        let p = PerfEvents::from_extra(&extra).unwrap();
        assert_eq!(p.fixed_period("ev"), Some(5));
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

    #[test]
    fn sample_weight_reads_the_weight_column_only_with_period_weights() {
        let w = [5.0, -1.0, f64::NAN, 2.5];
        assert_eq!(sample_weight(Some(&w), 0, true), 5);
        assert_eq!(sample_weight(Some(&w), 1, true), 0);
        assert_eq!(sample_weight(Some(&w), 2, true), 0);
        assert_eq!(sample_weight(Some(&w), 3, true), 2);
        assert_eq!(sample_weight(Some(&w), 0, false), 1);
        assert_eq!(sample_weight(None, 0, true), 1);
        assert_eq!(sample_weight(Some(&w), 9, true), 1);
    }

    #[test]
    fn marker_weight_prefers_period_then_fixed_period_then_one() {
        assert_eq!(marker_weight(Some(7.0), Some(100)), 7);
        assert_eq!(marker_weight(None, Some(100)), 100);
        assert_eq!(marker_weight(None, None), 1);
        assert_eq!(marker_weight(Some(-3.0), None), 1);
        assert_eq!(marker_weight(Some(f64::INFINITY), Some(100)), 100);
    }

    #[test]
    fn marker_weight_treats_a_zero_period_as_absent() {
        assert_eq!(marker_weight(Some(0.0), Some(100)), 100);
        assert_eq!(marker_weight(Some(0.0), None), 1);
    }

    #[test]
    fn marker_weight_treats_a_period_truncating_to_zero_as_absent() {
        assert_eq!(marker_weight(Some(0.5), Some(100)), 100);
        assert_eq!(marker_weight(Some(0.5), None), 1);
    }
}
