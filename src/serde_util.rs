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
