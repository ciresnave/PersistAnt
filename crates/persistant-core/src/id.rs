// SPDX-License-Identifier: MIT OR Apache-2.0
use std::fmt;

/// Why a string is not a capability id.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdError {
    /// The string is empty.
    Empty,
    /// A character outside `a-z 0-9 _ . :` at this byte offset.
    BadChar { at: usize },
    /// A leading, trailing or doubled `.`.
    BadDot,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::Empty => write!(f, "capability id is empty"),
            IdError::BadChar { at } => {
                write!(
                    f,
                    "capability id has a character outside a-z 0-9 _ . : at byte {at}"
                )
            }
            IdError::BadDot => write!(f, "capability id has a leading, trailing or doubled '.'"),
        }
    }
}

impl std::error::Error for IdError {}

/// A capability id such as `kv.prefix_list`, `cast.to_uuid` or `fn:max`.
///
/// Lower-case ASCII, digits, `_`, `.` as the namespace separator and `:` (as in `fn:max`). An id
/// names a behaviour or feature, never a backend, and never changes meaning: a change in meaning is
/// a new id. Ordering is plain string ordering so files diff stably.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityId(String);

impl CapabilityId {
    /// Validate and wrap an id.
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        if s.is_empty() {
            return Err(IdError::Empty);
        }
        let ok =
            |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b':');
        if let Some(at) = s.bytes().position(|b| !ok(b)) {
            return Err(IdError::BadChar { at });
        }
        if s.starts_with('.') || s.ends_with('.') || s.contains("..") {
            return Err(IdError::BadDot);
        }
        Ok(CapabilityId(s))
    }

    /// The id as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this id is `other` or sits beneath it by name (`kv.prefix_list.exactly_once` is
    /// under `kv.prefix_list`). Naming only: it says nothing about which implies which; that is a
    /// chain the vocabulary declares, not a string prefix.
    pub fn is_under(&self, other: &CapabilityId) -> bool {
        self.0 == other.0
            || (self.0.starts_with(&other.0) && self.0.as_bytes().get(other.0.len()) == Some(&b'.'))
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for CapabilityId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        CapabilityId::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_ids_in_the_design_and_in_unisql() {
        for s in [
            "kv.prefix_list.exactly_once",
            "cast.to_uuid",
            "fn:max",
            "fn:string_agg.order_by",
            "blob.max_value_size",
            "rel.source_dialect",
        ] {
            assert_eq!(CapabilityId::new(s).unwrap().as_str(), s);
        }
    }

    #[test]
    fn rejects_what_would_not_diff_or_parse() {
        assert_eq!(CapabilityId::new(""), Err(IdError::Empty));
        assert_eq!(CapabilityId::new("Kv.get"), Err(IdError::BadChar { at: 0 }));
        assert_eq!(CapabilityId::new("kv get"), Err(IdError::BadChar { at: 2 }));
        assert_eq!(CapabilityId::new("kv;get"), Err(IdError::BadChar { at: 2 }));
        assert_eq!(CapabilityId::new("kv..get"), Err(IdError::BadDot));
        assert_eq!(CapabilityId::new(".kv"), Err(IdError::BadDot));
        assert_eq!(CapabilityId::new("kv."), Err(IdError::BadDot));
    }

    #[test]
    fn is_under_matches_a_dot_boundary_only() {
        let a = CapabilityId::new("kv.prefix_list").unwrap();
        let b = CapabilityId::new("kv.prefix_list.exactly_once").unwrap();
        let c = CapabilityId::new("kv.prefix_listing").unwrap();
        assert!(b.is_under(&a) && a.is_under(&a));
        assert!(
            !c.is_under(&a),
            "control: a shared string prefix is not a namespace parent"
        );
        assert!(!a.is_under(&b));
    }
}
