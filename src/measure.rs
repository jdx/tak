//! Measurement backends.
//!
//! Two tiers, deliberately separated:
//!
//! - **Deterministic** (`instructions`) — reproducible to ~0.02% run-to-run and
//!   ~0.035% across wildly different machine load. This is the only tier that may
//!   gate CI.
//! - **Timing** (`wall_*`) — recorded and charted, never gated. On a quiet 32-core
//!   host wall clock still shows 4–20% coefficient of variation; under contention
//!   the median moves ~150%.
//!
//! Syscall counts and peak RSS sit awkwardly between the two: better than wall
//! clock (~1%) but not deterministic, because they move with thread scheduling.
//! They are recorded, and may be flagged, but must not gate at a tight threshold.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct Plan {
    pub cmd: Vec<String>,
    pub warmup: u32,
    pub runs: u32,
}

/// Run once, discarding output, returning elapsed wall time in milliseconds.
///
/// No shell. Spawning a shell adds its own startup cost and variance to every
/// sample, which for commands in the 10ms range is a large fraction of the
/// measurement — the same reasoning behind poop's refusal to support one.
fn time_once(cmd: &[String]) -> Result<f64> {
    let (bin, args) = cmd.split_first().context("empty command")?;
    let start = Instant::now();
    let status = Command::new(bin)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to spawn `{bin}`"))?;
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let _ = status;
    Ok(elapsed)
}

/// Wall-clock statistics over `plan.runs` samples.
///
/// Reports `min` alongside the mean because contention is one-sided — a busy
/// machine can only make a run slower, never faster — so the minimum is a far
/// more robust estimator than the mean on shared CI hardware.
pub fn wall(plan: &Plan) -> Result<BTreeMap<String, f64>> {
    for _ in 0..plan.warmup {
        time_once(&plan.cmd)?;
    }
    let mut samples = Vec::with_capacity(plan.runs as usize);
    for _ in 0..plan.runs {
        samples.push(time_once(&plan.cmd)?);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n as f64;
    let p50 = samples[n / 2];

    Ok(BTreeMap::from([
        ("wall_min_ms".to_string(), samples[0]),
        ("wall_p50_ms".to_string(), p50),
        ("wall_mean_ms".to_string(), mean),
        ("wall_max_ms".to_string(), samples[n - 1]),
        ("wall_n".to_string(), n as f64),
    ]))
}

/// Instruction count via `valgrind --tool=cachegrind`.
///
/// Returns `Ok(None)` when valgrind is unavailable rather than failing: this is
/// the expected state on macOS (no usable Apple Silicon support) and Windows.
/// Those platforms record timing only, and the CI gate lives on the Linux job.
/// Locally, a container gets you counters on any host.
pub fn instructions(cmd: &[String]) -> Result<Option<u64>> {
    if Command::new("valgrind")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        return Ok(None);
    }

    let out = Command::new("valgrind")
        .args([
            "--tool=cachegrind",
            "--cache-sim=no",
            "--branch-sim=no",
            "--cachegrind-out-file=/dev/null",
        ])
        .args(cmd)
        .stdout(Stdio::null())
        .output()
        .context("failed to run valgrind")?;

    // cachegrind writes its summary to stderr as e.g. "I refs:  48,349,132".
    let stderr = String::from_utf8_lossy(&out.stderr);
    Ok(parse_irefs(&stderr))
}

/// Extract the `I refs:` count from cachegrind's stderr summary.
fn parse_irefs(stderr: &str) -> Option<u64> {
    let line = stderr.lines().find(|l| l.contains("I refs:"))?;
    let digits: String = line
        .rsplit(':')
        .next()?
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cachegrind_summary() {
        let s = "==12== I refs:      48,349,132\n";
        assert_eq!(parse_irefs(s), Some(48_349_132));
    }

    #[test]
    fn missing_summary_is_none_not_panic() {
        assert_eq!(parse_irefs("valgrind: command not found"), None);
    }

    #[test]
    fn wall_reports_min_le_p50_le_max() {
        let plan = Plan {
            cmd: vec!["true".into()],
            warmup: 1,
            runs: 5,
        };
        let m = wall(&plan).unwrap();
        assert!(m["wall_min_ms"] <= m["wall_p50_ms"]);
        assert!(m["wall_p50_ms"] <= m["wall_max_ms"]);
        assert_eq!(m["wall_n"], 5.0);
    }
}
