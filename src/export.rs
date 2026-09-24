//! Results in hyperfine's `--export-json` shape.
//!
//! Projects moving from hyperfine already have scripts that read that file
//! (aube's `generate-results.js` reads `mean`, `stddev`, `min` and `max`).
//! Writing the same shape lets them switch the measuring tool without
//! rewriting what consumes it. `bench` and `subject` are extra keys, which
//! hyperfine consumers ignore; they are what tell entries apart once one file
//! holds several benchmarks. `user` and `system` are omitted because tak does
//! not measure CPU time. `checks` is present only for a subject with a
//! `check`, and leaves every hyperfine field as it would otherwise be.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Export {
    /// How the run was made, so a surprising result can be traced and its
    /// sample order repeated. Extra top-level keys; hyperfine has none.
    #[serde(flatten)]
    pub meta: Meta,
    pub results: Vec<ExportResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub tak_version: String,
    /// The run's seed: `tak run --seed` with it repeats the sample order, as
    /// long as `runs = "auto"` settles on the same counts. A string, because
    /// a seed given with `--seed` can exceed 2^53, and a JSON number that
    /// large is rounded by JavaScript and jq — silently naming another order.
    #[serde(serialize_with = "as_string")]
    pub seed: u64,
    /// The runner class the run would be recorded under.
    pub runner: String,
    /// When the run finished, RFC 3339.
    pub time: String,
}

#[derive(Debug, Serialize)]
pub struct ExportResult {
    /// hyperfine's display name: the subject, or the benchmark for a
    /// single-command one.
    pub command: String,
    pub bench: String,
    pub subject: String,
    /// Seconds, like hyperfine, not the milliseconds tak records.
    pub mean: f64,
    /// `null` for a single sample, as hyperfine writes it.
    pub stddev: Option<f64>,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    /// Every timed sample in seconds, in the order taken.
    pub times: Vec<f64>,
    /// What each sample exited with, aligned with `times`. Always one of the
    /// subject's `ok_exit_codes` (0 unless it says otherwise): a sample that
    /// exits with anything else drops its subject rather than being kept. A
    /// failed `check` is not a failed run, and is reported in `checks`.
    pub exit_codes: Vec<i32>,
    /// The outcome of the subject's `check`, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<Checks>,
}

/// How a subject's timed samples fared against its `check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checks {
    pub passed: usize,
    pub total: usize,
    /// Whether each sample passed, aligned with `times`, so a failure can be
    /// matched to the time it produced.
    pub samples: Vec<bool>,
}

impl Checks {
    pub fn new(samples: &[bool]) -> Self {
        Checks {
            passed: samples.iter().filter(|&&ok| ok).count(),
            total: samples.len(),
            samples: samples.to_vec(),
        }
    }
}

impl ExportResult {
    /// A result whose samples all exited 0; see [`Self::with_exit_codes`].
    pub fn new(bench: &str, subject: &str, command: &str, samples_ms: &[f64]) -> Self {
        let times: Vec<f64> = samples_ms.iter().map(|ms| ms / 1000.0).collect();
        let mut sorted = times.clone();
        sorted.sort_by(f64::total_cmp);
        let n = sorted.len();
        let mean = sorted.iter().sum::<f64>() / n as f64;
        let stddev = (n > 1).then(|| {
            (sorted.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
        });
        // The true median, as hyperfine reports it, rather than tak's p50
        // (the upper middle sample).
        let median = if n.is_multiple_of(2) {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        } else {
            sorted[n / 2]
        };
        ExportResult {
            command: command.to_string(),
            bench: bench.to_string(),
            subject: subject.to_string(),
            mean,
            stddev,
            median,
            min: sorted[0],
            max: sorted[n - 1],
            exit_codes: vec![0; n],
            times,
            checks: None,
        }
    }

    /// Record what each sample actually exited with, one per sample.
    pub fn with_exit_codes(mut self, codes: &[i32]) -> Self {
        debug_assert_eq!(codes.len(), self.times.len());
        self.exit_codes = codes.to_vec();
        self
    }

    /// Attach a subject's check outcomes, one per sample.
    pub fn with_checks(mut self, samples: &[bool]) -> Self {
        debug_assert_eq!(samples.len(), self.times.len());
        self.checks = Some(Checks::new(samples));
        self
    }
}

fn as_string<S: serde::Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

pub fn write(path: &Path, meta: Meta, results: Vec<ExportResult>) -> Result<()> {
    let json = serde_json::to_string_pretty(&Export { meta, results })?;
    std::fs::write(path, json + "\n").with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_seconds_and_the_median_is_the_true_median() {
        let r = ExportResult::new("b", "s", "s", &[4000.0, 1000.0, 3000.0, 2000.0]);
        assert_eq!(r.times, [4.0, 1.0, 3.0, 2.0], "kept in the order taken");
        assert_eq!((r.min, r.max, r.mean, r.median), (1.0, 4.0, 2.5, 2.5));
        assert_eq!(r.exit_codes, [0; 4]);
        assert!(r.stddev.unwrap() > 0.0);
    }

    /// `checks` is absent without a check, so a subject that has none
    /// exports exactly the hyperfine shape it did before.
    #[test]
    fn checks_are_exported_only_when_there_are_some() {
        let plain = serde_json::to_value(ExportResult::new("b", "s", "s", &[1.0, 2.0])).unwrap();
        assert!(plain.get("checks").is_none(), "{plain}");

        let checked = serde_json::to_value(
            ExportResult::new("b", "s", "s", &[1.0, 2.0, 3.0]).with_checks(&[true, false, true]),
        )
        .unwrap();
        assert_eq!(
            checked["checks"],
            serde_json::json!({"passed": 2, "total": 3, "samples": [true, false, true]})
        );
        assert_eq!(checked["exit_codes"], serde_json::json!([0, 0, 0]));
    }

    #[test]
    fn a_single_sample_has_no_stddev() {
        let r = ExportResult::new("b", "s", "s", &[5.0]);
        assert_eq!(r.stddev, None);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json["stddev"].is_null());
    }

    /// Each sample's real exit code is kept, in the order taken, rather than
    /// assumed to be 0.
    #[test]
    fn exit_codes_are_what_each_sample_exited_with() {
        let r = ExportResult::new("b", "s", "s", &[1.0, 2.0]).with_exit_codes(&[1, 0]);
        assert_eq!(r.exit_codes, [1, 0]);
    }
}
