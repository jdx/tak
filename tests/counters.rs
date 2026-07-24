//! Integration tests for the deterministic-metric path.
//!
//! `measure::instructions` was written, unit-tested at the parser level, and
//! then shipped without the cachegrind call ever executing once — valgrind was
//! simply absent from the machine it was developed on. These tests exist so CI
//! exercises the real subprocess, and so the determinism claim the whole project
//! rests on is asserted rather than assumed.
//!
//! Every test skips cleanly when valgrind is unavailable, because that is the
//! normal state on macOS and Windows.

use std::process::{Command, Stdio};

use tak_cli::measure::{self, Plan};

fn valgrind_available() -> bool {
    Command::new("valgrind")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A command that exists on any unix and does a trivial, fixed amount of work.
fn subject() -> Vec<String> {
    vec!["/bin/echo".to_string(), "tak".to_string()]
}

#[test]
fn instructions_are_reported_when_valgrind_exists() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let n = measure::instructions(&subject())
        .expect("cachegrind invocation failed")
        .expect("valgrind present but no I refs parsed");

    // A dynamically linked process cannot retire a trivially small number of
    // instructions; anything tiny means we parsed the wrong thing.
    assert!(n > 10_000, "implausibly low instruction count: {n}");
}

/// The claim the CI gate depends on: repeated runs of an identical command
/// return identical counts.
///
/// Wall clock on the same machine varies by 4-20%; if this metric drifted at
/// all, gating at 1% would be meaningless.
#[test]
fn instruction_counts_are_deterministic() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let cmd = subject();
    let runs: Vec<u64> = (0..3)
        .map(|_| {
            measure::instructions(&cmd)
                .expect("cachegrind invocation failed")
                .expect("no I refs parsed")
        })
        .collect();

    let min = *runs.iter().min().unwrap();
    let max = *runs.iter().max().unwrap();
    let spread = (max - min) as f64 / min as f64 * 100.0;

    assert!(
        spread < 0.1,
        "instruction counts varied by {spread:.4}% across {runs:?} — \
         the deterministic-gate premise does not hold on this platform"
    );
}

/// Absence of valgrind must degrade to timing-only, never fail the run.
#[test]
fn missing_valgrind_is_not_an_error() {
    if valgrind_available() {
        eprintln!("skipping: valgrind is installed, cannot test its absence");
        return;
    }
    let got = measure::instructions(&subject()).expect("must not error when valgrind is absent");
    assert!(got.is_none());
}

/// Wall-clock measurement works everywhere, with or without counters.
#[test]
fn wall_clock_works_without_counters() {
    let m = measure::wall(&Plan {
        cmd: subject(),
        warmup: 1,
        runs: 3,
    })
    .expect("wall measurement failed");

    assert_eq!(m["wall_n"], 3.0);
    assert!(m["wall_min_ms"] <= m["wall_p50_ms"]);
    assert!(m["wall_p50_ms"] <= m["wall_max_ms"]);
    assert!(m["wall_min_ms"] > 0.0);
}
