// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Value::Array` as a PostgreSQL array parameter, in the binary format, with the element type
//! chosen by the elements or by the statement's described type (`ArrayElem`).
//!
//! Nothing is guessed: the elements must all be of the kind the element type wants, a mixed list, a
//! nested array, or an empty or all-NULL list with no described type is a `RelError`, and a value
//! that does not fit (a uuid that is not a uuid) is one too. An array is built element by element
//! here because the server needs the element type in the array header, and `Value` carries a uuid
//! as text, so the type cannot come from the value alone.

use persistant_relational::{ArrayElem, RelError, Value};
use sqlx::postgres::{PgArgumentBuffer, PgTypeInfo, types::Oid};

/// (array type oid, element type oid) from `pg_type`.
fn oids(e: ArrayElem) -> (u32, u32) {
    match e {
        ArrayElem::Bool => (1000, 16),
        ArrayElem::Int8 => (1016, 20),
        ArrayElem::Float8 => (1022, 701),
        ArrayElem::Text => (1009, 25),
        ArrayElem::Bytea => (1001, 17),
        ArrayElem::Uuid => (2951, 2950),
        _ => unreachable!("every ArrayElem is handled; the enum is non_exhaustive for callers"),
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "Bool",
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::Text(_) => "Text",
        Value::Bytes(_) => "Bytes",
        Value::Array(_) => "Array",
        _ => "an unknown value",
    }
}

/// What the elements show: the first non-NULL one decides, the rest must agree.
fn infer(items: &[Value]) -> Result<ArrayElem, RelError> {
    let mut found: Option<(ArrayElem, &'static str)> = None;
    for v in items {
        let e = match v {
            Value::Null => continue,
            Value::Bool(_) => ArrayElem::Bool,
            Value::Int(_) => ArrayElem::Int8,
            Value::Float(_) => ArrayElem::Float8,
            Value::Text(_) => ArrayElem::Text,
            Value::Bytes(_) => ArrayElem::Bytea,
            other => {
                return Err(RelError::Unsupported(format!(
                    "an array parameter cannot hold {} elements",
                    kind(other)
                )));
            }
        };
        match found {
            None => found = Some((e, kind(v))),
            Some((first, name)) if first != e => {
                return Err(RelError::Unsupported(format!(
                    "an array parameter mixes {name} and {} elements",
                    kind(v)
                )));
            }
            Some(_) => {}
        }
    }
    found.map(|(e, _)| e).ok_or_else(|| {
        RelError::Unsupported(
            "the element type of an empty (or all-NULL) array parameter is not known: describe it \
             (`sqlx_query!` does, from sqlx's parameter type)"
                .into(),
        )
    })
}

fn uuid_bytes(s: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = s.bytes().filter(|b| *b != b'-').collect();
    // 32 hex digits, hyphens only where a uuid has them (or nowhere)
    let canonical = s.len() == 36
        && [8, 13, 18, 23].iter().all(|i| s.as_bytes()[*i] == b'-')
        && s.bytes().filter(|b| *b == b'-').count() == 4;
    if hex.len() != 32 || !(canonical || s.len() == 32) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, pair) in hex.chunks(2).enumerate() {
        let h = std::str::from_utf8(pair).ok()?;
        out[i] = u8::from_str_radix(h, 16).ok()?;
    }
    Some(out)
}

/// One element's bytes, or the reason it does not fit `elem`.
fn element(elem: ArrayElem, n: usize, v: &Value) -> Result<Vec<u8>, RelError> {
    let misfit = |want: &str| {
        RelError::Unsupported(format!(
            "array element {n} is {}, but the array is {want}",
            kind(v)
        ))
    };
    Ok(match (elem, v) {
        (ArrayElem::Bool, Value::Bool(b)) => vec![u8::from(*b)],
        (ArrayElem::Int8, Value::Int(i)) => i.to_be_bytes().to_vec(),
        (ArrayElem::Float8, Value::Float(f)) => f.to_bits().to_be_bytes().to_vec(),
        (ArrayElem::Text, Value::Text(s)) => s.as_bytes().to_vec(),
        (ArrayElem::Bytea, Value::Bytes(b)) => b.clone(),
        (ArrayElem::Uuid, Value::Text(s)) => uuid_bytes(s)
            .ok_or_else(|| {
                RelError::Unsupported(format!("array element {n} is not a uuid: {s:?}"))
            })?
            .to_vec(),
        (ArrayElem::Bool, _) => return Err(misfit("boolean[]")),
        (ArrayElem::Int8, _) => return Err(misfit("bigint[]")),
        (ArrayElem::Float8, _) => return Err(misfit("double precision[]")),
        (ArrayElem::Text, _) => return Err(misfit("text[]")),
        (ArrayElem::Bytea, _) => return Err(misfit("bytea[]")),
        (ArrayElem::Uuid, _) => return Err(misfit("uuid[]")),
        _ => unreachable!("every ArrayElem is handled"),
    })
}

/// A validated array, ready to bind.
pub(crate) struct PgArray {
    elem: ArrayElem,
    items: Vec<Option<Vec<u8>>>,
}

impl PgArray {
    /// Build from the elements and the statement's hint (or, with none, from what the elements show).
    pub(crate) fn new(items: &[Value], hint: Option<ArrayElem>) -> Result<Self, RelError> {
        let elem = match hint {
            Some(h) => h,
            None => infer(items)?,
        };
        let mut out = Vec::with_capacity(items.len());
        for (n, v) in items.iter().enumerate() {
            out.push(match v {
                Value::Null => None,
                other => Some(element(elem, n, other)?),
            });
        }
        Ok(PgArray { elem, items: out })
    }
}

impl sqlx::Type<sqlx::Postgres> for PgArray {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(Oid(0))
    }

    fn compatible(_: &PgTypeInfo) -> bool {
        true
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Postgres> for PgArray {
    fn produces(&self) -> Option<PgTypeInfo> {
        Some(PgTypeInfo::with_oid(Oid(oids(self.elem).0)))
    }

    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let elem_oid = oids(self.elem).1;
        let has_null = self.items.iter().any(Option::is_none);
        // header: number of dimensions, has-null flag, element type
        buf.extend_from_slice(&i32::from(!self.items.is_empty()).to_be_bytes());
        buf.extend_from_slice(&i32::from(has_null).to_be_bytes());
        buf.extend_from_slice(&elem_oid.to_be_bytes());
        if self.items.is_empty() {
            return Ok(sqlx::encode::IsNull::No);
        }
        let len = i32::try_from(self.items.len())?;
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&1i32.to_be_bytes()); // lower bound
        for item in &self.items {
            match item {
                None => buf.extend_from_slice(&(-1i32).to_be_bytes()),
                Some(bytes) => {
                    buf.extend_from_slice(&i32::try_from(bytes.len())?.to_be_bytes());
                    buf.extend_from_slice(bytes);
                }
            }
        }
        Ok(sqlx::encode::IsNull::No)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[Value], hint: Option<ArrayElem>) -> Result<ArrayElem, RelError> {
        PgArray::new(items, hint).map(|a| a.elem)
    }

    #[test]
    fn the_elements_decide_and_a_mix_is_an_error() {
        assert_eq!(
            v(&[Value::Int(1), Value::Null, Value::Int(3)], None).unwrap(),
            ArrayElem::Int8
        );
        assert_eq!(
            v(&[Value::Text("a".into())], None).unwrap(),
            ArrayElem::Text
        );
        assert!(matches!(
            v(&[Value::Int(1), Value::Text("a".into())], None),
            Err(RelError::Unsupported(_))
        ));
        assert!(matches!(
            v(&[Value::Int(1), Value::Float(1.0)], None),
            Err(RelError::Unsupported(_))
        ));
        assert!(matches!(
            v(&[Value::Array(vec![])], None),
            Err(RelError::Unsupported(_))
        ));
    }

    #[test]
    fn an_empty_array_needs_a_described_type() {
        assert!(matches!(v(&[], None), Err(RelError::Unsupported(_))));
        assert!(matches!(
            v(&[Value::Null], None),
            Err(RelError::Unsupported(_))
        ));
        assert_eq!(v(&[], Some(ArrayElem::Uuid)).unwrap(), ArrayElem::Uuid);
    }

    #[test]
    fn a_hint_never_casts() {
        // text elements do not become integers; ints do not become text
        assert!(v(&[Value::Text("1".into())], Some(ArrayElem::Int8)).is_err());
        assert!(v(&[Value::Int(1)], Some(ArrayElem::Text)).is_err());
        assert!(v(&[Value::Int(1)], Some(ArrayElem::Float8)).is_err());
        // a uuid hint wants real uuid text
        assert!(v(&[Value::Text("not-a-uuid".into())], Some(ArrayElem::Uuid)).is_err());
        let id = "11111111-2222-3333-4444-555555555555";
        assert!(v(&[Value::Text(id.into())], Some(ArrayElem::Uuid)).is_ok());
        assert!(v(&[Value::Text(id.replace('-', ""))], Some(ArrayElem::Uuid)).is_ok());
        assert!(
            v(
                &[Value::Text("1111-1111222233334444555555555555".into())],
                Some(ArrayElem::Uuid)
            )
            .is_err()
        );
    }

    #[test]
    fn the_binary_layout() {
        use sqlx::Encode;
        let mut buf = PgArgumentBuffer::default();
        let _ = PgArray::new(&[Value::Int(7), Value::Null], None)
            .unwrap()
            .encode_by_ref(&mut buf)
            .unwrap();
        let mut want = Vec::new();
        want.extend_from_slice(&1i32.to_be_bytes()); // one dimension
        want.extend_from_slice(&1i32.to_be_bytes()); // has a NULL
        want.extend_from_slice(&20u32.to_be_bytes()); // int8
        want.extend_from_slice(&2i32.to_be_bytes()); // length
        want.extend_from_slice(&1i32.to_be_bytes()); // lower bound
        want.extend_from_slice(&8i32.to_be_bytes());
        want.extend_from_slice(&7i64.to_be_bytes());
        want.extend_from_slice(&(-1i32).to_be_bytes());
        assert_eq!(&buf[..], &want[..]);
    }
}
