//! `tak log` against a real repository: notes inlined through `git log`, the
//! first-parent walk, and a shallow clone. The rendering is unit-tested in
//! `src/report.rs`; what can only be checked here is that the history git
//! hands back is the history the report describes.

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
        .output()
        .expect("failed to run tak")
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "tak failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn line(bench: &str, runner: &str, instructions: u64) -> String {
    format!(
        r#"{{"bench":"{bench}","metrics":{{"instructions":{instructions},"wall_min_ms":1.5}},"runner":"{runner}","tool":"self","ts":"2026-01-01T00:00:00Z","v":1}}"#
    )
}

fn note(dir: &Path, rev: &str, lines: &[String]) {
    git(
        dir,
        &[
            "notes",
            "--ref",
            "refs/notes/tak",
            "add",
            "-f",
            "-m",
            &lines.join("\n"),
            rev,
        ],
    );
}

fn commit(dir: &Path, subject: &str) -> String {
    git(dir, &["commit", "--quiet", "--allow-empty", "-m", subject]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A trunk of five commits, three of them recorded, plus a merged feature
/// branch whose own commit carries a measurement that must stay off the trunk.
fn trunk(tag: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("tak-log-{tag}-"))
        .tempdir()
        .unwrap();
    let d = dir.path();
    git(d, &["init", "--quiet", "-b", "main"]);

    let c1 = commit(d, "first");
    note(d, &c1, &[line("startup", "gha", 1_000_000)]);
    commit(d, "unrecorded");
    let c3 = commit(d, "feat: faster | leaner");
    note(
        d,
        &c3,
        // Two records for one series — a CI re-run — reduce to the minimum.
        &[
            line("startup", "gha", 990_000),
            line("startup", "gha", 1_500_000),
        ],
    );

    git(d, &["checkout", "--quiet", "-b", "feature"]);
    let side = commit(d, "side branch");
    note(d, &side, &[line("startup", "gha", 9_999_999)]);
    git(d, &["checkout", "--quiet", "main"]);
    git(
        d,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "merge feature",
            "feature",
        ],
    );
    let tip = git(d, &["rev-parse", "HEAD"]);
    note(
        d,
        &tip,
        &[
            line("startup", "gha", 1_010_000),
            line("resolve", "gha", 5_000),
        ],
    );
    dir
}

#[test]
fn the_log_walks_first_parent_history_newest_first() {
    let dir = trunk("walk");
    let out = stdout(&tak(dir.path(), &["log", "--no-credit"]));

    assert!(out.contains("3 recorded commit(s)"), "{out}");
    assert!(out.contains("### `startup` on `gha`"), "{out}");
    assert!(out.contains("### `resolve` on `gha`"), "{out}");
    // Newest first, each change measured from the previous measurement.
    let tip = out.find("1,010,000").expect("tip");
    let mid = out.find("990,000").expect("middle");
    let first = out.find("1,000,000").expect("first");
    assert!(tip < mid && mid < first, "{out}");
    assert!(out.contains("+2.02%"), "{out}");
    assert!(out.contains("-1.00%"), "{out}");
    // The re-run and the merged branch's own measurement are both absent.
    assert!(!out.contains("1,500,000"), "the minimum should win: {out}");
    assert!(!out.contains("9,999,999"), "a side branch leaked in: {out}");
    assert!(!out.contains("side branch"), "{out}");
    assert!(out.contains("feat: faster \\| leaner"), "{out}");
}

#[test]
fn the_limit_and_bench_filter_apply() {
    let dir = trunk("filter");
    let out = stdout(&tak(
        dir.path(),
        &["log", "-n", "2", "--bench", "startup", "--no-credit"],
    ));
    assert!(out.contains("2 recorded commit(s)"), "{out}");
    assert!(out.contains("1 older recorded commit(s)"), "{out}");
    assert!(!out.contains("resolve"), "{out}");
    assert!(!out.contains("1,000,000"), "{out}");

    let out = tak(dir.path(), &["log", "--bench", "nope"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`nope`") && err.contains("startup"), "{err}");
}

/// A user's `notes.displayRef` would add another ref's notes to `%N`. Records
/// under it are not tak's, and must not be read as though they were.
#[test]
fn other_notes_refs_stay_out() {
    let dir = trunk("display-ref");
    let d = dir.path();
    let head = git(d, &["rev-parse", "HEAD"]);
    git(
        d,
        &[
            "notes",
            "--ref",
            "refs/notes/commits",
            "add",
            "-m",
            &line("impostor", "gha", 1),
            &head,
        ],
    );
    git(d, &["config", "notes.displayRef", "refs/notes/*"]);
    let out = stdout(&tak(d, &["log"]));
    assert!(!out.contains("impostor"), "{out}");
}

#[test]
fn the_html_report_is_written() {
    let dir = trunk("html");
    let page = dir.path().join("report.html");
    let out = stdout(&tak(dir.path(), &["log", "--html", page.to_str().unwrap()]));
    assert!(out.contains("2 series over 3 recorded commit(s)"), "{out}");
    let html = std::fs::read_to_string(&page).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<svg"));
    assert!(html.contains("1,010,000 instructions"), "{html}");
}

/// `actions/checkout` clones one commit deep. A report built there has to say
/// that it saw a clone's history, not the project's.
#[test]
fn a_shallow_clone_says_so() {
    let dir = trunk("shallow");
    let origin = dir.path();
    let scratch = tempfile::tempdir().unwrap();
    let url = format!("file://{}", origin.display());
    git(
        scratch.path(),
        &["clone", "--quiet", "--depth", "1", &url, "shallow"],
    );
    let clone = scratch.path().join("shallow");

    // No local notes: `tak log` fetches them from origin itself.
    let out = stdout(&tak(&clone, &["log"]));
    assert!(out.contains("1 recorded commit(s)"), "{out}");
    assert!(out.contains("1 commit(s) walked"), "{out}");
    assert!(out.contains("shallow"), "{out}");
    assert!(out.contains("1,010,000"), "{out}");
}

#[test]
fn a_history_with_nothing_recorded_says_so() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet", "-b", "main"]);
    commit(dir.path(), "only");
    let out = stdout(&tak(dir.path(), &["log"]));
    assert!(out.contains("No measurements recorded"), "{out}");
}
