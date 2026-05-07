//! Tiny serde helpers shared across modules.
//!
//! Each predicate exists so a `#[serde(skip_serializing_if = "...")]`
//! attribute can keep numeric "default" fields out of the JSON when
//! they carry no information (e.g. zero counts on the happy path).

use std::io;

pub(crate) fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

pub(crate) fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// `io::Write` sink that only counts bytes.
///
/// Lets [`serialized_byte_count`] measure the JSON-rendered size of a
/// value without allocating the bytes themselves — the budget trimmer
/// in `tools::budget` calls this on every drop, so saving the
/// per-call `Vec<u8>` allocation matters at hundreds of rows.
struct ByteCount(usize);

impl io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Return the number of bytes `value` would occupy when serialized as
/// JSON, without materializing the bytes. Equivalent to
/// `serde_json::to_vec(value).map(|v| v.len()).unwrap_or(0)` at a
/// fraction of the allocation cost.
pub(crate) fn serialized_byte_count<T: serde::Serialize>(value: &T) -> usize {
    let mut ser = serde_json::Serializer::new(ByteCount(0));
    if value.serialize(&mut ser).is_err() {
        return 0;
    }
    ser.into_inner().0
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[test]
    fn byte_count_matches_to_string_len() {
        #[derive(Serialize)]
        struct Inner {
            a: u32,
            b: &'static str,
        }
        let value = vec![
            Inner { a: 1, b: "hello" },
            Inner {
                a: 9999,
                b: "world",
            },
        ];
        let counted = serialized_byte_count(&value);
        let actual = serde_json::to_string(&value).unwrap().len();
        assert_eq!(counted, actual);
    }

    #[test]
    fn byte_count_handles_strings() {
        let counted = serialized_byte_count(&vec!["3", "2", "1"]);
        let actual = serde_json::to_string(&vec!["3", "2", "1"]).unwrap().len();
        assert_eq!(counted, actual);
    }
}
