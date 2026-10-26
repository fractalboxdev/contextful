//! `store.bound-time` and `store.available-time`: the bounds a read carries and their echo.

use crate::store::declare::TableDecl;
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

/// Which vintage a read of a vintage table resolves per key
/// (`store.available-time.vintage-rows`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Vintage {
    /// The vintage with the latest availability instant the read admits.
    #[default]
    Latest,
    /// The vintage with the earliest availability instant the read admits.
    First,
}

/// The bounds one read carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bounds {
    pub as_of: Option<Bound>,
    pub valid_as_of: Option<Bound>,
    pub available_as_of: Option<Bound>,
    pub vintage: Vintage,
}

impl Bounds {
    /// The bounds selecting committed files: `as_of` alone, since valid and availability
    /// time narrow rows and never the file list.
    pub fn transaction(&self) -> Bounds {
        Bounds { as_of: self.as_of, ..Bounds::default() }
    }

    /// The bounds `decl`'s relation compiles under: valid and availability time only where
    /// it declares them, so a session registering every granted table refuses no read that
    /// never names an undeclaring one (`read.register.available-bound-argument`).
    pub fn for_table(&self, decl: &TableDecl) -> Bounds {
        Bounds {
            as_of: self.as_of,
            valid_as_of: self.valid_as_of.filter(|_| decl.valid_time.is_some()),
            available_as_of: self.available_as_of.filter(|_| decl.available_time.is_some()),
            vintage: self.vintage,
        }
    }

    /// `contextful.bounds` as a bounded read echoes it, or `None` for an unbounded read
    /// (`store.bound-time.echo`).
    pub fn echo(&self) -> Option<Value> {
        let given: Vec<Bound> = [self.as_of, self.valid_as_of, self.available_as_of].into_iter().flatten().collect();
        if given.is_empty() {
            return None;
        }
        let mut v = json!({});
        if let Some(b) = self.as_of {
            v["as_of"] = json!(b.at.to_rfc3339_nanos());
        }
        if let Some(b) = self.valid_as_of {
            v["valid_as_of"] = json!(b.at.to_rfc3339_nanos());
        }
        // `store.available-time.bound-echo`
        if let Some(b) = self.available_as_of {
            v["available_as_of"] = json!(b.at.to_rfc3339_nanos());
        }
        let inclusive = given.iter().all(|b| b.inclusive);
        v["inclusive"] = json!(inclusive);
        Some(v)
    }
}
