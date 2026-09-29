//! `tak compare`'s exit status is the gate, so it is tested on the real binary
//! rather than on `Comparison`: the unit tests already held the report to
//! saying "nothing was compared", and that sentence exited 0 for as long as
//! nobody asserted on the exit code.
//!
//! Notes are written with plain `git notes` rather than `tak run --record`, so
//! the instruction counts are chosen rather than measured and the tests run
//! without valgrind. Without that, the clean-comparison case would compare
//! only wall clock on most hosts, which never gates.

use std::path::Path;
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=tak-test", "-c", "user.email=t@example.com"])
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

fn tak(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(args)
        .current_dir(dir)
        // A gate percentage exported by the calling environment would change
        // what counts as a regression here.
        .env_remove("TAK_GATE_PCT")
        .output()
        .expect("failed to run tak")
}

/// A repository with two commits, `base` and `head`, and no remote: the
/// refresh from `origin` fails and compare falls back to the local notes,
/// which is the path a prefetched CI checkout takes too.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("tak-compare-")
        .tempdir()
        .unwrap();
    git(dir.path(), &["init", "--quiet", "-b", "main"]);
    git(
        dir.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "base"],
    );
    git(dir.path(), &["tag", "base"]);
    git(
        dir.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "head"],
    );
    dir
}

fn note(dir: &Path, rev: &str, runner: &str, instructions: u64) {
    let line = format!(
        r#"{{"bench":"startup","metrics":{{"instructions":{instructions},"wall_min_ms":1.0}},"runner":"{runner}","tool":"tak","ts":"2026-09-29T00:00:00Z","v":1}}"#
    );
    git(
        dir,
        &["notes", "--ref", "tak", "add", "-f", "-m", &line, rev],
    );
}

fn compare(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["compare", "base"];
    args.extend_from_slice(extra);
    tak(dir, &args)
}

/// The two states that produce an empty comparison in practice: a base that
/// was never recorded, and a runner-class migration.
fn empty_repos() -> Vec<(&'static str, tempfile::TempDir)> {
    let unrecorded = repo();
    note(unrecorded.path(), "HEAD", "ci-linux", 1_000_000);

    let migrated = repo();
    note(migrated.path(), "base", "ci-linux-old", 1_000_000);
    note(migrated.path(), "HEAD", "ci-linux-new", 1_000_000);

    vec![
        ("unrecorded base", unrecorded),
        ("runner migration", migrated),
    ]
}

#[test]
fn an_empty_comparison_fails_after_printing_the_report() {
    for (case, dir) in empty_repos() {
        let out = compare(dir.path(), &[]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{case}: passed with nothing compared"
        );
        // Workflows grep for this prefix; it is part of the interface.
        assert!(
            stdout.contains("**Nothing was compared"),
            "{case}: report missing: {stdout}"
        );
        // The report still names what did not line up — the useful half of
        // it when a runner class has just changed.
        assert!(
            stdout.contains("New, nothing to compare against"),
            "{case}: added list missing: {stdout}"
        );
        assert!(stderr.contains("--allow-empty"), "{case}: {stderr}");
    }
}

#[test]
fn allow_empty_and_no_gate_pass_an_empty_comparison() {
    for (case, dir) in empty_repos() {
        for flag in ["--allow-empty", "--no-gate"] {
            let out = compare(dir.path(), &[flag]);
            assert!(
                out.status.success(),
                "{case} with {flag} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("**Nothing was compared"),
                "{case} with {flag}: report missing"
            );
        }
    }
}

#[test]
fn a_clean_comparison_passes() {
    let dir = repo();
    note(dir.path(), "base", "ci-linux", 1_000_000);
    // 0.1%, inside the default 1% gate.
    note(dir.path(), "HEAD", "ci-linux", 1_001_000);
    let out = compare(dir.path(), &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "failed: {}\n{stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!stdout.contains("Nothing was compared"), "{stdout}");
}

/// `--allow-empty` forgives only the empty case. Were it to pass a regression
/// too, a workflow that set it once for a runner migration and forgot it would
/// have silently stopped gating.
#[test]
fn allow_empty_still_fails_a_regression() {
    let dir = repo();
    note(dir.path(), "base", "ci-linux", 1_000_000);
    note(dir.path(), "HEAD", "ci-linux", 1_100_000);
    let out = compare(dir.path(), &["--allow-empty"]);
    assert!(!out.status.success(), "a 10% rise passed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("regressed"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
