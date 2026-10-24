// SPDX-License-Identifier: MIT OR Apache-2.0
use std::collections::BTreeMap;
use std::fmt;

/// Why a `k=v;k=v` string is not a [`Config`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigError {
    /// A piece without `=` or with an empty key.
    BadPair(String),
    /// The same key twice.
    DuplicateKey(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::BadPair(p) => write!(f, "config piece {p:?} is not key=value"),
            ConfigError::DuplicateKey(k) => write!(f, "config key {k:?} appears twice"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Facts a gate can read at open, as `k=v;k=v` (a locale, a collation, `schema=none`).
///
/// Keys are kept sorted, so the string form is canonical and files diff stably. Values may not
/// contain `;` or a tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Config(BTreeMap<String, String>);

impl Config {
    /// The empty configuration.
    pub fn new() -> Self {
        Config::default()
    }

    /// Parse `k=v;k=v`. The empty string is the empty configuration.
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        let mut map = BTreeMap::new();
        if s.is_empty() {
            return Ok(Config(map));
        }
        for piece in s.split(';') {
            match piece.split_once('=') {
                Some((k, v)) if !k.is_empty() => {
                    if map.insert(k.to_string(), v.to_string()).is_some() {
                        return Err(ConfigError::DuplicateKey(k.to_string()));
                    }
                }
                _ => return Err(ConfigError::BadPair(piece.to_string())),
            }
        }
        Ok(Config(map))
    }

    /// Add or replace one fact.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }

    /// Whether every fact of `self` is also true of `actual`. A row recorded under
    /// `collation=utf8mb4_bin` holds for a backend whose readable facts include that one; a fact
    /// the row does not name is not constrained.
    pub fn holds_in(&self, actual: &Config) -> bool {
        self.0.iter().all(|(k, v)| actual.0.get(k) == Some(v))
    }

    /// The facts, sorted by key.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The value of one fact.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(";")?;
            }
            write!(f, "{k}={v}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_sorts() {
        let c = Config::parse("schema=none;collation=utf8mb4_bin").unwrap();
        assert_eq!(c.to_string(), "collation=utf8mb4_bin;schema=none");
        assert_eq!(Config::parse(&c.to_string()).unwrap(), c);
        assert_eq!(Config::parse("").unwrap(), Config::new());
    }

    #[test]
    fn rejects_malformed_pieces() {
        assert_eq!(Config::parse("a"), Err(ConfigError::BadPair("a".into())));
        assert_eq!(Config::parse("=v"), Err(ConfigError::BadPair("=v".into())));
        assert_eq!(
            Config::parse("a=1;a=2"),
            Err(ConfigError::DuplicateKey("a".into()))
        );
    }

    #[test]
    fn holds_in_is_a_subset_test_with_negative_controls() {
        let row = Config::parse("collation=utf8mb4_bin").unwrap();
        let actual = Config::parse("collation=utf8mb4_bin;schema=none").unwrap();
        assert!(row.holds_in(&actual));
        assert!(Config::new().holds_in(&actual));
        let other = Config::parse("collation=utf8mb4_0900_ai_ci").unwrap();
        assert!(
            !row.holds_in(&other),
            "control: a different value must not match"
        );
        assert!(
            !row.holds_in(&Config::new()),
            "control: an absent fact must not match"
        );
    }
}
