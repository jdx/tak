//! A benchmark subject must not inherit forge credentials.
//!
//! `measure.rs` unit-tests that the removal is *configured*. That is not the
//! same as proving a spawned process cannot see the variable, and the whole
//! point of this guarantee is what the child observes. These tests run the real
//! `tak` binary with a token set and let the subject report what it got.
//!
//! The token is set on the child rather than with `std::env::set_var`: mutating
//! the test process's environment races every other test in the binary.

use std::process::Command;

/// Run `tak run` with a token in the environment and a subject that writes the
/// value it observed, then return that value.
#[cfg(unix)]
fn observed_token(extra_args: &[&str]) -> String {
    let out = std::env::temp_dir().join(format!(
        "tak-subject-env-{}-{}",
        std::process::id(),
        extra_args.join("-").replace(['-', '/'], "_")
    ));
    let _ = std::fs::remove_file(&out);

    // `-` fallback rather than `:-`: an empty-but-present variable should read
    // as leaked, not as absent.
    let script = format!(
        "printf '%s' \"${{GITHUB_TOKEN-ABSENT}}\" > {}",
        out.display()
    );

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tak"));
    cmd.args(["run", "--runs", "1", "--warmup", "0"])
        .args(extra_args)
        .args(["--", "sh", "-c", &script])
        .env("GITHUB_TOKEN", "sentinel-must-not-leak")
        .env("GH_TOKEN", "sentinel-must-not-leak");

    let status = cmd.status().expect("failed to run tak");
    assert!(status.success(), "tak exited with {status}");

    let got = std::fs::read_to_string(&out).expect("subject wrote nothing");
    std::fs::remove_file(&out).ok();
    got
}

/// The wall-clock path spawns the subject directly.
#[cfg(unix)]
#[test]
fn the_timing_path_does_not_leak_the_token() {
    assert_eq!(observed_token(&["--no-counters"]), "ABSENT");
}

/// The cachegrind path spawns valgrind, which spawns the subject. Removing the
/// variable from valgrind has to carry through to what it runs — a separate
/// claim from the one above, and the one more likely to regress.
#[cfg(unix)]
#[test]
fn the_cachegrind_path_does_not_leak_the_token() {
    if !tak_cli::measure::valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    // Without --no-counters the subject runs under both paths; either one
    // leaking would write the sentinel, since the last writer wins and all
    // writers are the same subject.
    assert_eq!(observed_token(&[]), "ABSENT");
}

/// The setting is what drives the removal, not a compiled-in list. Allowing the
/// variable back has to reach all the way through the CLI to the spawned
/// process, or the setting is documentation rather than behaviour.
#[cfg(unix)]
#[test]
fn allowing_the_token_lets_it_through() {
    assert_eq!(
        observed_token(&["--no-counters", "--env-allow", "GITHUB_TOKEN"]),
        "sentinel-must-not-leak"
    );
}

/// Denying something else replaces the default list, so the token is no longer
/// among the removals and survives. This is the flag's documented semantics —
/// replace, not append — and the easiest part to get backwards.
#[cfg(unix)]
#[test]
fn denying_another_variable_replaces_the_default_list() {
    assert_eq!(
        observed_token(&["--no-counters", "--env-deny", "SOMETHING_ELSE"]),
        "sentinel-must-not-leak"
    );
}

/// The scrub must not blank out the wider environment — a subject that needs
/// PATH or HOME to run at all would otherwise fail in ways that look like a
/// broken benchmark.
#[cfg(unix)]
#[test]
fn unrelated_environment_still_reaches_the_subject() {
    let out = std::env::temp_dir().join(format!("tak-subject-env-keep-{}", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let script = format!(
        "printf '%s' \"${{TAK_TEST_KEEP-ABSENT}}\" > {}",
        out.display()
    );

    let status = Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(["run", "--runs", "1", "--warmup", "0", "--no-counters"])
        .args(["--", "sh", "-c", &script])
        .env("GITHUB_TOKEN", "sentinel-must-not-leak")
        .env("TAK_TEST_KEEP", "kept")
        .status()
        .expect("failed to run tak");
    assert!(status.success(), "tak exited with {status}");

    let got = std::fs::read_to_string(&out).expect("subject wrote nothing");
    std::fs::remove_file(&out).ok();
    assert_eq!(got, "kept");
}
