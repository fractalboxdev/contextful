//! The one timestamp grammar every checkpoint decodes.
//!
//! A textual timestamp is RFC 3339 with an explicit offset; it decodes into a UTC
//! instant, and checkpoints compare instants, never text (`authority.verify.timestamps`).
//! A numeric claim counts seconds since the Unix epoch and decodes into the same type.

use crate::AuthorityError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime, UtcOffset};

/// A UTC instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(OffsetDateTime);

impl Instant {
    /// Decode a textual timestamp, refusing anything outside the grammar with
    /// `TimestampMalformed` (`authority.verify.malformed-timestamp`).
    pub fn parse(s: &str) -> Result<Instant, AuthorityError> {
        // The RFC 3339 parser also takes a space between date and time; the grammar
        // admits `T` alone, so one instant has one spelling per offset.
        if s.as_bytes().get(10) != Some(&b'T') {
            return Err(AuthorityError::TimestampMalformed(format!(
                "`{s}` is not an RFC 3339 timestamp: date and time join with `T`"
            )));
        }
        OffsetDateTime::parse(s, &Rfc3339)
            .map(|t| Instant(t.to_offset(UtcOffset::UTC)))
            .map_err(|e| AuthorityError::TimestampMalformed(format!("`{s}` is not an RFC 3339 timestamp: {e}")))
    }

    /// Decode a numeric claim of seconds since the Unix epoch.
    pub fn from_unix_secs(secs: i64) -> Result<Instant, AuthorityError> {
        OffsetDateTime::from_unix_timestamp(secs)
            .map(Instant)
            .map_err(|e| AuthorityError::TimestampMalformed(format!("{secs} is not a representable instant: {e}")))
    }

    /// Seconds since the Unix epoch.
    pub fn unix_secs(self) -> i64 {
        self.0.unix_timestamp()
    }

    /// The instant `secs` seconds later, saturating at the largest representable one.
    pub fn plus_secs(self, secs: u64) -> Instant {
        let secs = i64::try_from(secs).unwrap_or(i64::MAX);
        match self.0.checked_add(Duration::seconds(secs)) {
            Some(t) => Instant(t),
            None => Instant(time::PrimitiveDateTime::MAX.assume_utc()),
        }
    }

    /// Whole seconds from `self` to `later`, zero when `later` is not after `self`.
    pub fn secs_until(self, later: Instant) -> u64 {
        u64::try_from((later.0 - self.0).whole_seconds()).unwrap_or(0)
    }

    /// The canonical textual form: RFC 3339 in UTC, `Z`-suffixed.
    pub fn to_rfc3339(self) -> String {
        self.0.format(&Rfc3339).expect("a UTC instant formats as RFC 3339")
    }
}

impl std::fmt::Display for Instant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl Serialize for Instant {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for Instant {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Instant::parse(&s).map_err(serde::de::Error::custom)
    }
}
