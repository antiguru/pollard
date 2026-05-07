//! Tiny serde helpers shared across modules.
//!
//! Each predicate exists so a `#[serde(skip_serializing_if = "...")]`
//! attribute can keep numeric "default" fields out of the JSON when
//! they carry no information (e.g. zero counts on the happy path).

pub(crate) fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

pub(crate) fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// Serialize an `f32` rounded to one decimal place. Used on the `*_pct`
/// columns where 0.0–100.0 with one fractional digit is plenty for an
/// LLM to reason about. Cuts ~5 bytes per pct field on average vs the
/// default full-precision rendering — adds up across hundreds of rows
/// in a single response (#101).
pub(crate) fn round1_pct<S: serde::Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
    let rounded = (v * 10.0).round() / 10.0;
    s.serialize_f32(rounded)
}

/// Round an `Option<f64>` (millisecond columns) to two decimals.
/// Time deltas of 0.01 ms are well below profiling noise but two
/// decimals are kept so small movements still register.
pub(crate) fn round2_ms_opt<S: serde::Serializer>(
    v: &Option<f64>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) => {
            let rounded = (x * 100.0).round() / 100.0;
            s.serialize_some(&rounded)
        }
        None => s.serialize_none(),
    }
}
