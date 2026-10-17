// SPDX-License-Identifier: MIT OR Apache-2.0
//! Column types the portable [`Value`](persistant_relational::Value) has no variant for, carried as
//! **canonical text** instead of refusing them: a `uuid` as lowercase hyphenated text, a `numeric` or
//! `DECIMAL` as its exact decimal digits (never a float), dates and times as ISO 8601, JSON as its
//! text. Text is the one form that loses nothing and compares the way a consumer already holds these
//! values. The types with no canonical text here still fail loud, naming the type
//! (`docs/DECODER-AUDIT.md` has the whole table).
//!
//! These are the pure formatters; each driver decides which column types reach them.

use sqlx::types::time::{Date, OffsetDateTime, PrimitiveDateTime, Time};
use std::fmt::Write;

/// A 4-byte float as the shortest decimal that reads back as it: `0.1f32` is `0.1`, not the
/// `0.10000000149011612` that widening to `f64` gives. This is what the server itself prints.
pub(crate) fn f32_shortest(f: f32) -> f64 {
    f.to_string().parse().unwrap_or_else(|_| f64::from(f))
}

/// `a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11` from 16 bytes.
pub(crate) fn uuid_text(b: &[u8]) -> Result<String, String> {
    let b: &[u8; 16] = b
        .try_into()
        .map_err(|_| format!("a uuid is 16 bytes, got {}", b.len()))?;
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        let _ = write!(s, "{byte:02x}");
    }
    Ok(s)
}

/// PostgreSQL's binary `numeric` as its exact decimal text, with the column's scale kept
/// (`12345.67`, `-0.50`, `0.00`); `NaN` and the infinities as PostgreSQL prints them.
pub(crate) fn numeric_text(b: &[u8]) -> Result<String, String> {
    let bad = || format!("malformed numeric ({} bytes)", b.len());
    if b.len() < 8 {
        return Err(bad());
    }
    let i16at = |i: usize| i16::from_be_bytes([b[i], b[i + 1]]);
    let ndigits = usize::try_from(i16at(0)).map_err(|_| bad())?;
    let weight = i32::from(i16at(2));
    let sign = u16::from_be_bytes([b[4], b[5]]);
    let dscale = usize::try_from(i16at(6)).map_err(|_| bad())?;
    match sign {
        0xC000 => return Ok("NaN".to_string()),
        0xD000 => return Ok("Infinity".to_string()),
        0xF000 => return Ok("-Infinity".to_string()),
        0x0000 | 0x4000 => {}
        _ => return Err(bad()),
    }
    if b.len() != 8 + 2 * ndigits {
        return Err(bad());
    }
    let digit = |i: i32| -> u16 {
        // Base-10000 digit `i` counted from the first stored digit; outside the stored run it is 0.
        usize::try_from(i)
            .ok()
            .filter(|&i| i < ndigits)
            .map_or(0, |i| u16::from_be_bytes([b[8 + 2 * i], b[9 + 2 * i]]))
    };
    let mut s = String::new();
    if sign == 0x4000 {
        s.push('-');
    }
    // Integer part: base-10000 digits from `weight` down to 0.
    if weight < 0 {
        s.push('0');
    } else {
        for w in 0..=weight {
            let d = digit(w);
            if w == 0 {
                let _ = write!(s, "{d}");
            } else {
                let _ = write!(s, "{d:04}");
            }
        }
    }
    if dscale > 0 {
        s.push('.');
        let mut frac = String::new();
        let mut w = weight + 1;
        while frac.len() < dscale {
            let _ = write!(frac, "{:04}", digit(w));
            w += 1;
        }
        frac.truncate(dscale);
        s.push_str(&frac);
    }
    Ok(s)
}

/// `inet` or `cidr` from PostgreSQL's binary form. An `inet` host address prints without a prefix,
/// as PostgreSQL does; a `cidr`, or an `inet` with a shorter prefix, prints `address/bits`.
pub(crate) fn inet_text(b: &[u8]) -> Result<String, String> {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let bad = || format!("malformed inet ({} bytes)", b.len());
    if b.len() < 4 {
        return Err(bad());
    }
    let (family, bits, is_cidr, n) = (b[0], b[1], b[2] != 0, usize::from(b[3]));
    let addr = b.get(4..4 + n).ok_or_else(bad)?;
    let (text, full) = match (family, n) {
        (2, 4) => (
            Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string(),
            32,
        ),
        (3, 16) => {
            let a: [u8; 16] = addr.try_into().map_err(|_| bad())?;
            (Ipv6Addr::from(a).to_string(), 128)
        }
        _ => return Err(bad()),
    };
    Ok(if is_cidr || bits != full {
        format!("{text}/{bits}")
    } else {
        text
    })
}

fn fraction(s: &mut String, micros: u32) {
    if micros != 0 {
        let f = format!("{micros:06}");
        s.push('.');
        s.push_str(f.trim_end_matches('0'));
    }
}

/// `2026-10-17`
pub(crate) fn date_text(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// `12:34:56` or `12:34:56.789` (the fraction only when there is one, trailing zeros trimmed).
pub(crate) fn time_text(t: Time) -> String {
    let mut s = format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second());
    fraction(&mut s, t.microsecond());
    s
}

/// `2026-10-17T12:34:56.789`, no zone.
pub(crate) fn naive_text(t: PrimitiveDateTime) -> String {
    format!("{}T{}", date_text(t.date()), time_text(t.time()))
}

/// `2026-10-17T10:34:56.789Z`, always UTC.
pub(crate) fn utc_text(t: OffsetDateTime) -> String {
    let u = t.to_offset(sqlx::types::time::UtcOffset::UTC);
    format!(
        "{}Z",
        naive_text(PrimitiveDateTime::new(u.date(), u.time()))
    )
}

#[cfg(feature = "mysql")]
/// MySQL `TIME`, which runs -838:59:59 to 838:59:59: `-838:59:59`, `01:02:03.456`.
pub(crate) fn mysql_time_text(t: &sqlx::mysql::types::MySqlTime) -> String {
    let mut s = format!(
        "{}{:02}:{:02}:{:02}",
        // `MySqlTime::is_negative` returns `is_positive` in sqlx 0.9.0: ask the sign itself.
        if t.sign().is_negative() { "-" } else { "" },
        t.hours(),
        t.minutes(),
        t.seconds()
    );
    fraction(&mut s, t.microseconds());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric(ndigits: i16, weight: i16, sign: u16, dscale: i16, digits: &[u16]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [ndigits, weight] {
            b.extend(v.to_be_bytes());
        }
        b.extend(sign.to_be_bytes());
        b.extend(dscale.to_be_bytes());
        for d in digits {
            b.extend(d.to_be_bytes());
        }
        b
    }

    #[test]
    fn numerics() {
        // 12345.67 = 1 | 2345 . 6700, weight 1, dscale 2
        assert_eq!(
            numeric_text(&numeric(3, 1, 0, 2, &[1, 2345, 6700])).unwrap(),
            "12345.67"
        );
        assert_eq!(
            numeric_text(&numeric(2, 0, 0x4000, 2, &[0, 5000])).unwrap(),
            "-0.50"
        );
        assert_eq!(numeric_text(&numeric(0, 0, 0, 2, &[])).unwrap(), "0.00");
        assert_eq!(numeric_text(&numeric(0, 0, 0, 0, &[])).unwrap(), "0");
        // 10000 = one digit 1 at weight 1
        assert_eq!(numeric_text(&numeric(1, 1, 0, 0, &[1])).unwrap(), "10000");
        // 0.000001234 = groups 0000|0123|4000: stored as [123, 4000], weight -2, dscale 9
        assert_eq!(
            numeric_text(&numeric(2, -2, 0, 9, &[123, 4000])).unwrap(),
            "0.000001234"
        );
        assert_eq!(numeric_text(&numeric(0, 0, 0xC000, 0, &[])).unwrap(), "NaN");
        assert!(numeric_text(&[0; 4]).is_err());
        assert!(numeric_text(&numeric(2, 0, 0, 0, &[1])).is_err());
    }

    #[test]
    fn uuids_and_floats() {
        let b = [
            0xa0, 0xee, 0xbc, 0x99, 0x9c, 0x0b, 0x4e, 0xf8, 0xbb, 0x6d, 0x6b, 0xb9, 0xbd, 0x38,
            0x0a, 0x11,
        ];
        assert_eq!(
            uuid_text(&b).unwrap(),
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"
        );
        assert!(uuid_text(&b[..15]).is_err());
        assert_eq!(f32_shortest(0.1), 0.1);
        assert_eq!(f32_shortest(f32::MAX), 3.4028235e38);
        assert!(f32_shortest(f32::NAN).is_nan());
    }

    #[test]
    fn inets() {
        assert_eq!(inet_text(&[2, 32, 0, 4, 10, 0, 0, 1]).unwrap(), "10.0.0.1");
        assert_eq!(
            inet_text(&[2, 24, 0, 4, 10, 0, 0, 1]).unwrap(),
            "10.0.0.1/24"
        );
        assert_eq!(
            inet_text(&[2, 24, 1, 4, 10, 0, 0, 0]).unwrap(),
            "10.0.0.0/24"
        );
        let mut v6 = vec![3, 128, 0, 16];
        v6.extend([0u8; 15]);
        v6.push(1);
        assert_eq!(inet_text(&v6).unwrap(), "::1");
        assert!(inet_text(&[2, 32, 0, 4, 10]).is_err());
    }

    #[test]
    fn dates_and_times() {
        let d = Date::from_ordinal_date(2026, 290).unwrap();
        assert_eq!(date_text(d), "2026-10-17");
        let t = Time::from_hms_micro(12, 34, 56, 789_000).unwrap();
        assert_eq!(time_text(t), "12:34:56.789");
        assert_eq!(time_text(Time::from_hms(12, 34, 56).unwrap()), "12:34:56");
        assert_eq!(
            naive_text(PrimitiveDateTime::new(d, t)),
            "2026-10-17T12:34:56.789"
        );
        let at = PrimitiveDateTime::new(d, t)
            .assume_offset(sqlx::types::time::UtcOffset::from_hms(2, 0, 0).unwrap());
        assert_eq!(utc_text(at), "2026-10-17T10:34:56.789Z");
    }
}
