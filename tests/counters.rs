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

use tak_cli::measure::{self, Plan};

use tak_cli::measure::valgrind_available;

/// A command that exists on the host and does a trivial, fixed amount of work.
///
/// Absolute path on unix so no shell or PATH lookup is involved; on Windows
/// there is no `/bin/echo`, and no cachegrind either, so the counter tests skip
/// and only the wall-clock test needs a working subject.
fn subject() -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["/bin/echo".to_string(), "tak".to_string()]
    }
    #[cfg(windows)]
    {
        vec!["cmd".to_string(), "/C".to_string(), "echo tak".to_string()]
    }
}

#[test]
fn instructions_are_reported_when_valgrind_exists() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let c = measure::instructions(&subject(), None)
        .expect("cachegrind invocation failed")
        .expect("valgrind present but no I refs parsed");

    // A dynamically linked process cannot retire a trivially small number of
    // instructions; anything tiny means we parsed the wrong thing.
    assert!(
        c.min > 10_000,
        "implausibly low instruction count: {}",
        c.min
    );
    assert!(c.max >= c.min);
    assert!(
        c.runs >= 2,
        "a single sample cannot detect a varying subject"
    );
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
    // `instructions` already samples repeatedly; doing it again covers variation
    // across separate invocations too, not just within one.
    let outer: Vec<_> = (0..2)
        .map(|_| {
            measure::instructions(&cmd, None)
                .expect("cachegrind invocation failed")
                .expect("no I refs parsed")
        })
        .collect();

    let min = outer.iter().map(|c| c.min).min().unwrap();
    let max = outer.iter().map(|c| c.max).max().unwrap();
    let spread = (max - min) as f64 / min as f64 * 100.0;

    assert!(
        spread < 0.1,
        "instruction counts varied by {spread:.4}% — the deterministic-gate \
         premise does not hold on this platform"
    );
    // /bin/echo is hermetic, so nothing here should look suspect.
    assert!(outer.iter().all(|c| !c.is_suspect()));
}

/// The two "no counters" outcomes must stay distinguishable: a missing valgrind
/// is `Ok(None)`, while valgrind failing mid-measurement is an error. Reporting
/// the latter as the former sends people installing what they already have.
#[test]
fn availability_and_failure_are_distinct() {
    if valgrind_available() {
        // A command that cannot be spawned makes cachegrind fail rather than
        // vanish, so this must be an error rather than Ok(None).
        let bogus = vec!["/nonexistent/tak-not-a-real-binary".to_string()];
        assert!(
            measure::instructions(&bogus, None).is_err(),
            "a failed measurement must not look like a missing valgrind"
        );
    } else {
        assert!(measure::instructions(&subject(), None).unwrap().is_none());
    }
}

/// Absence of valgrind must degrade to timing-only, never fail the run.
#[test]
fn missing_valgrind_is_not_an_error() {
    if valgrind_available() {
        eprintln!("skipping: valgrind is installed, cannot test its absence");
        return;
    }
    let got =
        measure::instructions(&subject(), None).expect("must not error when valgrind is absent");
    assert!(got.is_none());
}

/// Wall-clock measurement works everywhere, with or without counters.
#[test]
fn wall_clock_works_without_counters() {
    let m = measure::wall(&Plan {
        cmd: subject(),
        warmup: 1,
        runs: 3,
        dir: None,
    })
    .expect("wall measurement failed");

    assert_eq!(m["wall_n"], 3.0);
    assert!(m["wall_min_ms"] <= m["wall_p50_ms"]);
    assert!(m["wall_p50_ms"] <= m["wall_max_ms"]);
    assert!(m["wall_min_ms"] > 0.0);
}
