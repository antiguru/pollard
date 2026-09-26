//! Decoder for the JSLB (JsonSlabs) container.
//!
//! A JSLB file holds a root JSON skeleton plus binary slabs, and the
//! skeleton refers to slabs with `{"$s": N}` placeholders. The
//! `json-slabs` crate parses the container but leaves substitution to
//! the consumer, so this module rebuilds the plain JSON value.

use json_slabs::{DecodeError, ParsedFile, SLAB_REF_KEY, SlabPlaceholder, SlabType};
use serde_json::{Map, Number, Value};

pub(crate) const MAGIC: [u8; 8] = json_slabs::MAGIC;

pub(crate) fn to_value(bytes: &[u8]) -> Result<Value, String> {
    let file = ParsedFile::parse(bytes).map_err(|e| e.to_string())?;
    let root: Value =
        serde_json::from_slice(file.root_json_bytes()).map_err(|e| format!("root slab: {e}"))?;
    substitute(&file, root)
}

fn substitute(file: &ParsedFile<'_>, value: Value) -> Result<Value, String> {
    match value {
        Value::Object(map) => {
            if let Some(idx) = placeholder_index(&map) {
                return slab_value(file, SlabPlaceholder(idx));
            }
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, substitute(file, v)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => items
            .into_iter()
            .map(|v| substitute(file, v))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other),
    }
}

/// A placeholder is an object whose only member is `$s`.
fn placeholder_index(map: &Map<String, Value>) -> Option<usize> {
    if map.len() != 1 {
        return None;
    }
    map.get(SLAB_REF_KEY)?
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
}

fn slab_value(file: &ParsedFile<'_>, p: SlabPlaceholder) -> Result<Value, String> {
    let err = |e: DecodeError| e.to_string();
    let slab_type = file.slab_at(p).map_err(err)?.slab_type;
    Ok(match slab_type {
        SlabType::Json => {
            let bytes = file.read_subjson_bytes(p).map_err(err)?;
            let v: Value =
                serde_json::from_slice(bytes).map_err(|e| format!("slab {}: {e}", p.index()))?;
            return substitute(file, v);
        }
        SlabType::Int8 => ints(file.read::<i8>(p).map_err(err)?),
        SlabType::Uint8 => ints(file.read::<u8>(p).map_err(err)?),
        SlabType::Int16 => ints(file.read::<i16>(p).map_err(err)?),
        SlabType::Uint16 => ints(file.read::<u16>(p).map_err(err)?),
        SlabType::Int32 => ints(file.read::<i32>(p).map_err(err)?),
        SlabType::Uint32 => ints(file.read::<u32>(p).map_err(err)?),
        SlabType::Int64 => ints(file.read::<i64>(p).map_err(err)?),
        SlabType::Uint64 => ints(file.read::<u64>(p).map_err(err)?),
        SlabType::Float32 => floats(file.read::<f32>(p).map_err(err)?.into_iter().map(f64::from)),
        SlabType::Float64 => floats(file.read::<f64>(p).map_err(err)?.into_iter()),
    })
}

fn ints<T: Into<Number>>(values: Vec<T>) -> Value {
    Value::Array(
        values
            .into_iter()
            .map(|v| Value::Number(v.into()))
            .collect(),
    )
}

/// Non-finite floats have no JSON form and become `null`.
fn floats(values: impl Iterator<Item = f64>) -> Value {
    Value::Array(
        values
            .map(|v| Number::from_f64(v).map_or(Value::Null, Value::Number))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use json_slabs::Builder;
    use serde_json::json;

    #[test]
    fn substitutes_every_numeric_slab_type() {
        let mut b = Builder::new();
        let i8s = b.add_slab_from_vec(vec![-1i8, 2]);
        let u8s = b.add_slab_from_vec(vec![255u8]);
        let i16s = b.add_slab_from_vec(vec![-300i16]);
        let u16s = b.add_slab_from_vec(vec![60000u16]);
        let i32s = b.add_slab_from_vec(vec![-70000i32]);
        let u32s = b.add_slab_from_vec(vec![4_000_000_000u32]);
        let f32s = b.add_slab_from_vec(vec![0.5f32]);
        let f64s = b.add_slab_from_vec(vec![1.25f64]);
        let i64s = b.add_slab_from_vec(vec![-5_000_000_000i64]);
        let u64s = b.add_slab_from_vec(vec![10_000_000_000u64]);
        let root = format!(
            r#"{{"a":{i8s:#},"b":{u8s:#},"c":{i16s:#},"d":{u16s:#},"e":{i32s:#},"f":{u32s:#},"g":{f32s:#},"h":{f64s:#},"i":{i64s:#},"j":{u64s:#}}}"#
        );
        let bytes = b.finish(root.as_bytes());
        let v = to_value(&bytes).unwrap();
        assert_eq!(
            v,
            json!({"a": [-1, 2], "b": [255], "c": [-300], "d": [60000], "e": [-70000],
                   "f": [4_000_000_000u64], "g": [0.5], "h": [1.25],
                   "i": [-5_000_000_000i64], "j": [10_000_000_000u64]})
        );
    }

    #[test]
    fn substitutes_nested_json_slabs() {
        let mut b = Builder::new();
        let inner_numbers = b.add_slab_from_vec(vec![1i32, 2, 3]);
        let inner = b.add_json_slab(format!(r#"{{"xs":{inner_numbers:#},"s":"hi"}}"#).into_bytes());
        let root = format!(r#"{{"threads":[{inner:#}],"keep":{{"$s":0,"other":1}}}}"#);
        let bytes = b.finish(root.as_bytes());
        let v = to_value(&bytes).unwrap();
        // An object with keys besides `$s` is not a placeholder.
        assert_eq!(
            v,
            json!({"threads": [{"xs": [1, 2, 3], "s": "hi"}], "keep": {"$s": 0, "other": 1}})
        );
    }

    #[test]
    fn out_of_range_placeholder_is_an_error() {
        let b = Builder::new();
        let bytes = b.finish(br#"{"x":{"$s":9}}"#);
        let err = to_value(&bytes).unwrap_err();
        assert!(err.contains("slab index 9 out of range"), "{err}");
    }
}
