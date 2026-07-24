//! The on-disk record format.
//!
//! One JSON object per line, keys sorted, stored as a git note on the commit the
//! measurement describes. Line-oriented and order-independent so that git's
//! `cat_sort_uniq` notes merge strategy resolves concurrent CI writers without
//! conflicts — see [`crate::notes`].
//!
//! Serialisation goes through [`Record::to_line`] rather than plain
//! `serde_json::to_string` because `cat_sort_uniq` dedupes on exact bytes: two
//! writers emitting the same measurement must produce byte-identical lines.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Bumped when the record shape changes incompatibly. Readers skip lines whose
/// version they do not understand rather than failing the whole history.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// Schema version. Serialised first-by-name (`v`) purely by BTreeMap ordering.
    pub v: u32,

    /// Benchmark name, from `bench.toml`.
    pub bench: String,

    /// Which program produced this measurement. Defaults to the project itself;
    /// set to `pnpm`, `npm`, … for competitive comparisons, which are wall-clock
    /// only — instruction counts across different programs are not comparable.
    pub tool: String,

    /// Version of `tool`, when meaningful (release backfills, competitors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Runner class. Series MUST be partitioned on this: moving between runner
    /// types shifts absolute numbers enough to look like a regression.
    pub runner: String,

    /// RFC 3339, second resolution.
    pub ts: String,

    /// Metric name -> value. Deterministic metrics (`instructions`) are integers;
    /// timings are milliseconds. A BTreeMap so key order is stable across writers.
    pub metrics: BTreeMap<String, f64>,
}

impl Record {
    /// Serialise to a single line, with map keys sorted.
    ///
    /// Stability matters: `cat_sort_uniq` dedupes byte-identical lines, so an
    /// identical measurement written twice must collapse to one entry.
    pub fn to_line(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Parse one line. Returns `Ok(None)` for blank lines and for records from a
    /// future schema version, so that a newer writer cannot break an older reader.
    pub fn from_line(line: &str) -> anyhow::Result<Option<Self>> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        let rec: Record = serde_json::from_str(line)?;
        if rec.v > SCHEMA_VERSION {
            return Ok(None);
        }
        Ok(Some(rec))
    }
}

/// Parse a whole note body, skipping lines that fail to parse.
///
/// A single malformed line — hand-edited, or written by a broken tool — must not
/// render an entire commit's history unreadable.
pub fn parse_note(body: &str) -> Vec<Record> {
    body.lines()
        .filter_map(|l| Record::from_line(l).ok().flatten())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> Record {
        Record {
            v: SCHEMA_VERSION,
            bench: "hook-env".into(),
            tool: "mise".into(),
            version: None,
            runner: "linux-amd64".into(),
            ts: "2026-07-24T09:00:00Z".into(),
            metrics: BTreeMap::from([
                ("instructions".to_string(), 29_413_235.0),
                ("wall_min_ms".to_string(), 11.28),
            ]),
        }
    }

    #[test]
    fn line_roundtrips() {
        let line = rec().to_line().unwrap();
        let back = Record::from_line(&line).unwrap().unwrap();
        assert_eq!(back.bench, "hook-env");
        assert_eq!(back.metrics["instructions"], 29_413_235.0);
    }

    #[test]
    fn serialisation_is_byte_stable() {
        // cat_sort_uniq dedupes on exact bytes; two writers must agree.
        assert_eq!(rec().to_line().unwrap(), rec().to_line().unwrap());
    }

    #[test]
    fn blank_lines_are_skipped_not_errors() {
        assert!(Record::from_line("   ").unwrap().is_none());
    }

    #[test]
    fn future_schema_versions_are_skipped() {
        let mut r = rec();
        r.v = SCHEMA_VERSION + 1;
        let line = r.to_line().unwrap();
        assert!(Record::from_line(&line).unwrap().is_none());
    }

    #[test]
    fn one_bad_line_does_not_poison_the_note() {
        let body = format!(
            "{}\nnot json at all\n{}",
            rec().to_line().unwrap(),
            rec().to_line().unwrap()
        );
        assert_eq!(parse_note(&body).len(), 2);
    }
}
