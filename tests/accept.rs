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

fn compare(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out: Output = Command::new(env!("CARGO_BIN_EXE_tak"))
        .arg("compare")
        .args(args)
        // No such remote: the refresh fails fast and the local notes are read.
        .args(["--remote", "no-such-remote"])
        .env_remove("TAK_GATE_PCT")
        .env_remove("TAK_CREDIT")
        .current_dir(dir)
        .output()
        .expect("failed to run tak");
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
    let (ok, stdout, stderr) = compare(dir.path(), &[&base]);
    assert!(
        !ok,
        "`resolve` was not accepted and must still fail: {stdout}"
    );
    assert!(stderr.contains("1 benchmark(s) regressed"), "{stderr}");
    assert!(
        stdout.contains(&format!(
            "`startup` +10.00% (`Tak-Accept` in `{}`)",
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
    let (ok, stdout, _) = compare(dir.path(), &[&base]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains("**2 accepted regression(s)"), "{stdout}");
    assert!(
        stdout.contains("no benchmark by that name was compared on both sides: `other`"),
        "{stdout}"
    );
}

#[test]
fn the_flag_accepts_without_a_trailer() {
    let (dir, base, _) = regressed("slower");
    let (ok, stdout, _) = compare(
        dir.path(),
        &[&base, "--accept", "startup", "--accept", "resolve"],
    );
    assert!(ok, "{stdout}");
    assert!(
        stdout.contains("`startup` +10.00% (`--accept`)"),
        "{stdout}"
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
    let (ok, stdout, _) = compare(dir.path(), &[&base]);
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
    let (ok, stdout, _) = compare(dir.path(), &[&base]);
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
    let (ok, stdout, _) = compare(dir.path(), &[&base]);
    assert!(!ok, "{stdout}");
    assert!(!stdout.contains("accepted"), "{stdout}");
}
