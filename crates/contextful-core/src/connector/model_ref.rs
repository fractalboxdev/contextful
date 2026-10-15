//! A model reference (`connector.infer.cutoffs`): the pinned snapshot a call reaches and
//! the months its pre-training and post-training knowledge end, each tagged with how the
//! month is known.

use crate::time::Instant;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// How a cutoff month is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoffTag {
    /// The provider states the month.
    Documented,
    /// The month is inferred from the model's behavior.
    Estimated,
}

/// A knowledge cutoff: a calendar month and its tag, spelled `2024-10:documented`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cutoff {
    pub year: u16,
    pub month: u8,
    pub tag: CutoffTag,
}

impl Cutoff {
    /// Read `YYYY-MM:documented` or `YYYY-MM:estimated`.
    pub fn parse(s: &str) -> Result<Cutoff, String> {
        let bad = || format!("`{s}` is no cutoff; spell it as a month and its tag, as `2024-10:documented`");
        let (month, tag) = s.split_once(':').ok_or_else(bad)?;
        let tag = match tag {
            "documented" => CutoffTag::Documented,
            "estimated" => CutoffTag::Estimated,
            _ => return Err(bad()),
        };
        let (y, m) = month.split_once('-').ok_or_else(bad)?;
        if y.len() != 4 || m.len() != 2 {
            return Err(bad());
        }
        let year: u16 = y.parse().map_err(|_| bad())?;
        let month: u8 = m.parse().map_err(|_| bad())?;
        if !(1..=12).contains(&month) {
            return Err(bad());
        }
        Ok(Cutoff { year, month, tag })
    }

    /// The instant the cutoff month ends: the start of the following month, UTC.
    pub fn end(&self) -> Instant {
        let (year, month) = if self.month == 12 { (self.year + 1, 1) } else { (self.year, self.month + 1) };
        Instant::parse(&format!("{year:04}-{month:02}-01T00:00:00Z")).expect("a calendar month start is an instant")
    }
}

impl fmt::Display for Cutoff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self.tag {
            CutoffTag::Documented => "documented",
            CutoffTag::Estimated => "estimated",
        };
        write!(f, "{:04}-{:02}:{tag}", self.year, self.month)
    }
}

impl Serialize for Cutoff {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Cutoff {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Cutoff, D::Error> {
        Cutoff::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// The model a call reaches: its pinned snapshot id and its two cutoffs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub snapshot: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pretrain_cutoff: Option<Cutoff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posttrain_cutoff: Option<Cutoff>,
}

impl ModelRef {
    /// A reference to `snapshot` declaring no cutoff.
    pub fn snapshot(snapshot: impl Into<String>) -> ModelRef {
        ModelRef { snapshot: snapshot.into(), pretrain_cutoff: None, posttrain_cutoff: None }
    }

    /// Where pre-training knowledge ends for a call at `call`; undeclared, the call itself.
    pub fn pretrain_end(&self, call: Instant) -> Instant {
        self.pretrain_cutoff.map_or(call, |c| c.end())
    }

    /// Where post-training knowledge ends for a call at `call`; undeclared, the call itself.
    pub fn posttrain_end(&self, call: Instant) -> Instant {
        self.posttrain_cutoff.map_or(call, |c| c.end())
    }
}
