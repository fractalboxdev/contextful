//! `store.bound-time`: the two bounds a read carries and their echo.

use crate::time::Instant;
use crate::AuthorityError;
use serde_json::{json, Value};

/// An upper bound on one clock. `inclusive` admits the bound instant itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound {
    pub at: Instant,
    pub inclusive: bool,
}

impl Bound {
    /// Parse a bound literal: an RFC 3339 instant, inclusive; or a date-only
    /// `YYYY-MM-DD`, resolved here to the start of the next day, exclusive
    /// (`store.bound-time.instant-comparison`).
    pub fn parse(s: &str) -> Result<Bound, AuthorityError> {
        let b = s.as_bytes();
        let date_only = b.len() == 10 && b[4] == b'-' && b[7] == b'-';
        if date_only {
            let start = Instant::parse(&format!("{s}T00:00:00Z"))?;
            return Ok(Bound { at: start.plus_secs(86_400), inclusive: false });
        }
        Ok(Bound { at: Instant::parse(s)?, inclusive: true })
    }

    /// Whether `t` falls at or before the bound, compared as instants.
    pub fn admits(&self, t: Instant) -> bool {
        if self.inclusive {
            t <= self.at
        } else {
            t < self.at
        }
    }
}

/// The bounds one read carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bounds {
    pub as_of: Option<Bound>,
    pub valid_as_of: Option<Bound>,
}

impl Bounds {
    /// `contextful.bounds` as a bounded read echoes it, or `None` for an unbounded read
    /// (`store.bound-time.echo`).
    pub fn echo(&self) -> Option<Value> {
        if self.as_of.is_none() && self.valid_as_of.is_none() {
            return None;
        }
        let mut v = json!({});
        if let Some(b) = self.as_of {
            v["as_of"] = json!(b.at.to_rfc3339_nanos());
        }
        if let Some(b) = self.valid_as_of {
            v["valid_as_of"] = json!(b.at.to_rfc3339_nanos());
        }
        let inclusive = self.as_of.iter().chain(self.valid_as_of.iter()).all(|b| b.inclusive);
        v["inclusive"] = json!(inclusive);
        Some(v)
    }
}
