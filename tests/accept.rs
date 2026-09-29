//! `tak compare` must honour an accepted regression, and only that one.
//!
//! The unit tests in `compare.rs` cover the gating semantics on records built
//! in memory. What they cannot see is the part that reads commit messages: which
//! commits count as "the change", and what git will and will not parse as a
//! trailer. Both are decided by a `git log` invocation, so these drive the real
//! binary against a real repository.

use std::path::Path;
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=tak-test",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet", "-b", "main"]);
    dir
}

/// Commit with `message` and return the new SHA.
fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["commit", "--quiet", "--allow-empty", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Attach a measurement to `sha` directly, the way a recording would, without
/// needing valgrind to produce an instruction count.
fn note(dir: &Path, sha: &str, benches: &[(&str, f64)]) {
    let lines: Vec<String> = benches
        .iter()
        .map(|(bench, ins)| {
            format!(
                r#"{{"bench":"{bench}","metrics":{{"instructions":{ins},"wall_min_ms":1.0}},"runner":"test","tool":"self","ts":"2026-01-01T00:00:00Z","v":1}}"#
            )
        })
        .collect();
    git(
        dir,
        &[
            "notes",
            "--ref=tak",
            "add",
            "-f",
            "-m",
            &lines.join("\n"),
            sha,
        ],
    );
}

/// `tak compare` with trailers at their default: not honoured.
fn compare(dir: &Path, args: &[&str]) -> (bool, String, String) {
    run_compare(dir, args, None)
}

/// `tak compare` with `TAK_ACCEPT_TRAILERS=1`.
fn compare_trusting(dir: &Path, args: &[&str]) -> (bool, String, String) {
    run_compare(dir, args, Some("1"))
}

fn run_compare(dir: &Path, args: &[&str], trailers: Option<&str>) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tak"));
    cmd.arg("compare")
        .args(args)
        // No such remote: the refresh fails fast and the local notes are read.
        .args(["--remote", "no-such-remote"])
        .env_remove("TAK_GATE_PCT")
        .env_remove("TAK_CREDIT")
        .env_remove("TAK_ACCEPT_TRAILERS")
        .current_dir(dir);
    if let Some(value) = trailers {
        cmd.env("TAK_ACCEPT_TRAILERS", value);
    }
    let out: Output = cmd.output().expect("failed to run tak");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A base with two benchmarks, and a head `message` that regresses both by 10%.
fn regressed(message: &str) -> (tempfile::TempDir, String, String) {
    let dir = repo();
    let base = commit(dir.path(), "base");
    note(dir.path(), &base, &[("startup", 1e6), ("resolve", 1e6)]);
    let head = commit(dir.path(), message);
    note(dir.path(), &head, &[("startup", 1.1e6), ("resolve", 1.1e6)]);
    (dir, base, head)
}

#[test]
fn an_unaccepted_regression_fails() {
    let (dir, base, _) = regressed("slower");
    let (ok, stdout, stderr) = compare(dir.path(), &[&base]);
    assert!(!ok, "{stdout}");
    assert!(
        stdout.contains("**2 benchmark(s) above the 1% gate:**"),
        "{stdout}"
    );
    assert!(stderr.contains("2 benchmark(s) regressed"), "{stderr}");
}

#[test]
fn a_trailer_accepts_only_the_benchmark_it_names() {
    let (dir, base, head) = regressed("slower startup\n\nTak-Accept: startup");
    let (ok, stdout, stderr) = compare_trusting(dir.path(), &[&base]);
    assert!(
        !ok,
        "`resolve` was not accepted and must still fail: {stdout}"
    );
    assert!(stderr.contains("1 benchmark(s) regressed"), "{stderr}");
    assert!(
        stdout.contains(&format!(
            "`startup` on `test` +10.00% (`Tak-Accept` in `{}`)",
            &head[..12]
        )),
        "{stdout}"
    );
}

/// Comma-separated and repeated trailers both count, and git matches the
/// key case-insensitively.
#[test]
fn trailers_may_be_repeated_or_listed() {
    let (dir, base, _) = regressed("slower\n\nTak-Accept: startup\ntak-accept: resolve, other");
    let (ok, stdout, _) = compare_trusting(dir.path(), &[&base]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains("**2 accepted regression(s)"), "{stdout}");
    assert!(
        stdout.contains("no benchmark by that name was compared on both sides: `other`"),
        "{stdout}"
    );
}

/// The default. A pull request's own commits are the change being gated, so
/// their trailers must not waive the gate unless the project opted in — and
/// the report says the trailer was seen, so nobody wonders why it did nothing.
#[test]
fn trailers_are_ignored_by_default_and_the_report_says_so() {
    let (dir, base, head) = regressed("slower startup\n\nTak-Accept: startup");
    let (ok, stdout, stderr) = compare(dir.path(), &[&base]);
    assert!(!ok, "{stdout}");
    assert!(stderr.contains("2 benchmark(s) regressed"), "{stderr}");
    assert!(!stdout.contains("(accepted)"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "trailers were found but not honoured, because `gate.accept_trailers` is off: \
             `startup` (`Tak-Accept` in `{}`)",
            &head[..12]
        )),
        "{stdout}"
    );
}

/// No trailers, no line: the note exists to explain a trailer, not to
/// advertise the setting on every report.
#[test]
fn no_trailers_means_no_note() {
    let (dir, base, _) = regressed("slower");
    let (_, stdout, _) = compare(dir.path(), &[&base]);
    assert!(!stdout.contains("not honoured"), "{stdout}");
}

/// Opting in from `tak.toml`, and opting back out from the environment. The
/// file is read from the checkout under test, so a workflow that does not
/// trust the change needs a way to override it.
#[test]
fn the_config_opts_in_and_the_environment_overrides_it() {
    let (dir, base, _) = regressed("slower\n\nTak-Accept: startup, resolve");
    std::fs::write(
        dir.path().join("tak.toml"),
        "[gate]\naccept_trailers = true\n",
    )
    .unwrap();
    let (ok, stdout, _) = compare(dir.path(), &[&base]);
    assert!(ok, "tak.toml opted in: {stdout}");
    assert!(stdout.contains("**2 accepted regression(s)"), "{stdout}");

    let (ok, stdout, _) = run_compare(dir.path(), &[&base], Some("0"));
    assert!(!ok, "the environment opted out: {stdout}");
    assert!(stdout.contains("not honoured"), "{stdout}");
}

/// `--accept` does not depend on the setting: it comes from whoever runs the
/// comparison, not from the commits being compared.
#[test]
fn the_flag_accepts_without_a_trailer() {
    let (dir, base, _) = regressed("slower");
    let (ok, stdout, _) = compare(
        dir.path(),
        &[&base, "--accept", "startup", "--accept", "resolve"],
    );
    assert!(ok, "{stdout}");
    assert!(
        stdout.contains("`startup` on `test` +10.00% (`--accept`)"),
        "{stdout}"
    );
}

/// Benchmark names are unrestricted, so `--accept` takes each value as one
/// exact name. Splitting it on commas left a name like this with no spelling
/// that could accept it.
#[test]
fn the_flag_accepts_a_name_containing_a_comma() {
    let dir = repo();
    let base = commit(dir.path(), "base");
    note(dir.path(), &base, &[("parse a,b", 1e6)]);
    let head = commit(dir.path(), "slower");
    note(dir.path(), &head, &[("parse a,b", 1.1e6)]);
    let (ok, stdout, _) = compare(dir.path(), &[&base, "--accept", "parse a,b"]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains("**1 accepted regression(s)"), "{stdout}");
}

/// `" startup "` and `startup` are different benchmarks. Trimming the flag
/// value accepted the wrong one's regression and left the named one failing.
#[test]
fn the_flag_accepts_only_the_exact_name() {
    let dir = repo();
    let base = commit(dir.path(), "base");
    note(dir.path(), &base, &[(" startup ", 1e6), ("startup", 1e6)]);
    let head = commit(dir.path(), "slower");
    note(
        dir.path(),
        &head,
        &[(" startup ", 1.1e6), ("startup", 1.1e6)],
    );
    let (ok, stdout, stderr) = compare(dir.path(), &[&base, "--accept", " startup "]);
    assert!(
        !ok,
        "`startup` was not accepted and must still fail: {stdout}"
    );
    assert!(stderr.contains("1 benchmark(s) regressed"), "{stderr}");
    assert!(
        stdout.contains("**1 benchmark(s) above the 1% gate:** `startup` +10.00%"),
        "{stdout}"
    );
    assert!(
        stdout.contains("` startup ` on `test` +10.00% (`--accept`)"),
        "{stdout}"
    );
}

/// An empty value is almost always an unset variable in a CI script. Failing
/// says so; skipping it would quietly accept nothing.
#[test]
fn an_empty_flag_value_is_an_error() {
    let (dir, base, _) = regressed("slower");
    let (ok, _, stderr) = compare(dir.path(), &[&base, "--accept", ""]);
    assert!(!ok);
    assert!(
        stderr.contains("--accept needs a benchmark name"),
        "{stderr}"
    );
}

/// Only the compared range speaks for the change. A trailer that landed before
/// the base was about some earlier change, and must not keep a benchmark
/// ungated forever after.
#[test]
fn a_trailer_before_the_base_does_not_count() {
    let dir = repo();
    commit(dir.path(), "long ago\n\nTak-Accept: startup");
    let base = commit(dir.path(), "base");
    note(dir.path(), &base, &[("startup", 1e6)]);
    let head = commit(dir.path(), "slower");
    note(dir.path(), &head, &[("startup", 1.1e6)]);
    let (ok, stdout, _) = compare_trusting(dir.path(), &[&base]);
    assert!(!ok, "{stdout}");
    assert!(!stdout.contains("accepted"), "{stdout}");
}

/// A merge-commit workflow: the trailer is on a branch commit, reachable only
/// through the merge's second parent. Walking first-parent only would miss it.
#[test]
fn a_trailer_on_a_merged_branch_counts() {
    let dir = repo();
    let base = commit(dir.path(), "base");
    note(dir.path(), &base, &[("startup", 1e6)]);
    git(dir.path(), &["checkout", "--quiet", "-b", "feature"]);
    let branch = commit(dir.path(), "slower startup\n\nTak-Accept: startup");
    git(dir.path(), &["checkout", "--quiet", "main"]);
    commit(dir.path(), "unrelated");
    git(
        dir.path(),
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "merge feature",
            "feature",
        ],
    );
    let merge = git(dir.path(), &["rev-parse", "HEAD"]);
    note(dir.path(), &merge, &[("startup", 1.1e6)]);
    let (ok, stdout, _) = compare_trusting(dir.path(), &[&base]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains(&branch[..12]), "{stdout}");
}

/// git reads trailers from the final paragraph only. A `Tak-Accept:` line
/// buried in the body — where a squash merge that concatenates commit messages
/// can leave it — is prose, not a trailer, and the gate fails.
#[test]
fn a_trailer_outside_the_final_paragraph_is_not_one() {
    let (dir, base, _) =
        regressed("squashed\n\n* slower startup\n\nTak-Accept: startup\n\n* another commit");
    let (ok, stdout, _) = compare_trusting(dir.path(), &[&base]);
    assert!(!ok, "{stdout}");
    assert!(!stdout.contains("accepted"), "{stdout}");
}

/// An acceptance waives a regression; it is not a way to pass a comparison
/// that compared nothing. With no series on both sides, `tak compare` fails
/// whatever was accepted, and the acceptance is reported as naming nothing.
#[test]
fn an_acceptance_does_not_pass_an_empty_comparison() {
    let dir = repo();
    let base = commit(dir.path(), "base");
    let head = commit(dir.path(), "slower");
    note(dir.path(), &head, &[("startup", 1.1e6)]);
    let (ok, stdout, stderr) = compare(dir.path(), &[&base, "--accept", "startup"]);
    assert!(!ok, "{stdout}");
    assert!(stderr.contains("nothing was compared"), "{stderr}");
    assert!(
        stdout.contains("no benchmark by that name was compared on both sides: `startup`"),
        "{stdout}"
    );
}

/// Acceptance layers on the per-benchmark gates in `tak.toml`: each series is
/// judged against its own gate first. `startup` at 20% did not regress, so
/// accepting it accepted nothing; `resolve` at the global 1% did, and a
/// report-only `help` needs no acceptance to pass.
#[test]
fn acceptance_uses_each_benchmarks_own_gate() {
    let dir = repo();
    std::fs::write(
        dir.path().join("tak.toml"),
        "[bench.startup]\ncmd = \"true\"\ngate = { pct = 20.0 }\n\n\
         [bench.resolve]\ncmd = \"true\"\n\n\
         [bench.help]\ncmd = \"true\"\ngate = { enabled = false }\n",
    )
    .unwrap();
    let base = commit(dir.path(), "base");
    note(
        dir.path(),
        &base,
        &[("startup", 1e6), ("resolve", 1e6), ("help", 1e6)],
    );
    let head = commit(dir.path(), "slower");
    note(
        dir.path(),
        &head,
        &[("startup", 1.1e6), ("resolve", 1.1e6), ("help", 1.1e6)],
    );
    let (ok, stdout, _) = compare(
        dir.path(),
        &[&base, "--accept", "resolve", "--accept", "startup"],
    );
    assert!(ok, "{stdout}");
    assert!(
        stdout.contains("`resolve` on `test` +10.00% (gate 1%) (`--accept`)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("not above its gate, so nothing was accepted: `startup`"),
        "{stdout}"
    );
    assert!(stdout.contains("**+10.00%** (not gated)"), "{stdout}");
}
