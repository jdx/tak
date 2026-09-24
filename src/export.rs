//! Results in hyperfine's `--export-json` shape.
//!
//! Projects moving from hyperfine already have scripts that read that file
//! (aube's `generate-results.js` reads `mean`, `stddev`, `min` and `max`).
//! Writing the same shape lets them switch the measuring tool without
//! rewriting what consumes it. `bench` and `subject` are extra keys, which
//! hyperfine consumers ignore; they are what tell entries apart once one file
//! holds several benchmarks. `user` and `system` are omitted because tak does
//! not measure CPU time.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Export {
    pub results: Vec<ExportResult>,
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
    /// Always zero: a sample that fails drops its subject rather than being
    /// kept, so every exported time is from a successful run.
    pub exit_codes: Vec<i32>,
}

impl ExportResult {
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
        }
    }
}

pub fn write(path: &Path, results: Vec<ExportResult>) -> Result<()> {
    let json = serde_json::to_string_pretty(&Export { results })?;
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

    #[test]
    fn a_single_sample_has_no_stddev() {
        let r = ExportResult::new("b", "s", "s", &[5.0]);
        assert_eq!(r.stddev, None);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json["stddev"].is_null());
    }
}
