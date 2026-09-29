//! `tak detect` against a real repository with notes on some of its commits.
//!
//! The step rules are unit-tested in `src/detect.rs`. What only a repository
//! can show is the walk: first-parent order, commits with no note becoming part
//! of a range, the window counting recorded commits, and the exit status a
//! main-branch workflow would see.
//!
//! Notes are written directly rather than measured, so this runs without
//! valgrind and the instruction counts are exactly the ones asserted on.

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
        // No remote exists, so the refresh fails and falls back to local notes,
        // as it does offline. Pointing at a name keeps it from guessing.
        .args(["--remote", "nowhere", "--no-credit"])
        // The default gate, whatever the environment running the tests says.
        .env_remove("TAK_GATE_PCT")
        .current_dir(dir)
        .output()
        .expect("failed to run tak")
}

fn line(bench: &str, runner: &str, instructions: u64) -> String {
    format!(
        r#"{{"bench":"{bench}","metrics":{{"instructions":{instructions}.0,"wall_min_ms":1.0}},"runner":"{runner}","tool":"self","ts":"2026-01-01T00:00:00Z","v":1}}"#
    )
}

/// c0..c7 on one first-parent line. `startup` on `gha`:
///
/// ```text
/// c0 1000   c1 1000   c2 —   c3 1100   c4 1100   c5 1105   c6 1110   c7 1115
/// ```
///
/// A 10% step somewhere in c2..c3, then a creep of 1.36% in steps of ~0.45%.
/// c7 also carries the first measurement on a second runner.
fn repo() -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::Builder::new()
        .prefix("tak-detect-")
        .tempdir()
        .unwrap();
    let d = dir.path();
    git(d, &["init", "--quiet", "-b", "main"]);
    let values = [
        Some(1000),
        Some(1000),
        None,
        Some(1100),
        Some(1100),
        Some(1105),
        Some(1110),
        Some(1115),
    ];
    let mut shas = Vec::new();
    for (i, v) in values.iter().enumerate() {
        git(
            d,
            &["commit", "--quiet", "--allow-empty", "-m", &format!("c{i}")],
        );
        let sha = git(d, &["rev-parse", "HEAD"]);
        if let Some(v) = v {
            let mut body = line("startup", "gha", *v);
            if i == 7 {
                body = format!("{body}\n{}", line("startup", "bigger", 9000));
            }
            git(d, &["notes", "--ref", "refs/notes/tak", "add", "-m", &body]);
        }
        shas.push(sha);
    }
    (dir, shas)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn short(sha: &str) -> &str {
    &sha[..12]
}

/// The run for the commit that introduced a step fails, and names the range
/// it could have come from, because c2 was never measured.
#[test]
fn a_step_onto_the_head_fails_and_names_its_range() {
    let (dir, c) = repo();
    let out = tak(dir.path(), &["detect", &c[3]]);
    let md = stdout(&out);
    assert!(!out.status.success(), "a 10% step must fail: {md}");
    assert!(
        md.contains(&format!("`{}..{}` (2 commits)", short(&c[1]), short(&c[3]))),
        "{md}"
    );
    assert!(md.contains("**+10.00%** ⚠️"), "{md}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stepped up by more than 1%"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = tak(dir.path(), &["detect", &c[3], "--no-gate"]);
    assert!(out.status.success(), "--no-gate reports without failing");
}

/// The next commit passes: a main workflow fails once, on the commit that
/// caused the step, not on every push after it.
#[test]
fn the_commit_after_a_step_passes_and_still_reports_it() {
    let (dir, c) = repo();
    let out = tak(dir.path(), &["detect", &c[4]]);
    let md = stdout(&out);
    assert!(out.status.success(), "{md}");
    assert!(md.contains("Earlier steps above the 1% gate"), "{md}");
    assert!(
        md.contains(&format!("`{}..{}`", short(&c[1]), short(&c[3]))),
        "{md}"
    );
    assert!(md.contains(&format!("| `{}` |", short(&c[4]))), "{md}");
}

/// Creep made of sub-threshold steps is reported from after the last step,
/// and never fails. The second runner is a new series, not a 700% step.
#[test]
fn drift_and_runner_changes_are_reported_not_failed() {
    let (dir, c) = repo();
    let out = tak(dir.path(), &["detect"]);
    let md = stdout(&out);
    assert!(out.status.success(), "{md}");
    assert!(md.contains("Sustained drift"), "{md}");
    assert!(
        md.contains(&format!(
            "+1.36% (1,100 → 1,115) over 5 recorded commits since `{}`",
            short(&c[3])
        )),
        "{md}"
    );
    assert!(
        md.contains("nothing to compare against: `startup` on `bigger`"),
        "{md}"
    );
    assert!(md.contains("8 first-parent commit(s)"), "{md}");
}

/// The window counts recorded commits. Two of them hold one step and no room
/// for drift or for the step further back.
#[test]
fn the_window_bounds_what_is_examined() {
    let (dir, _) = repo();
    let md = stdout(&tak(dir.path(), &["detect", "--window", "2"]));
    assert!(md.contains("2 first-parent commit(s)"), "{md}");
    assert!(!md.contains("Sustained drift"), "{md}");
    assert!(!md.contains("Earlier steps"), "{md}");

    let out = tak(dir.path(), &["detect", "--window", "1"]);
    assert!(!out.status.success(), "a window of one cannot hold a step");
}

/// A head with nothing recorded compares nothing and must say so, rather than
/// read like a pass.
#[test]
fn an_unrecorded_head_says_nothing_was_compared() {
    let (dir, c) = repo();
    let out = tak(dir.path(), &["detect", &c[2]]);
    let md = stdout(&out);
    assert!(out.status.success(), "{md}");
    assert!(md.contains("**Nothing was compared"), "{md}");
    assert!(md.contains("No instruction counts are recorded"), "{md}");
}

/// First-parent only: a merged side branch's measurements are not trunk
/// points, and a step on the branch is not a step on main.
#[test]
fn merged_branches_are_not_walked() {
    let (dir, _) = repo();
    let d = dir.path();
    git(d, &["checkout", "--quiet", "-b", "side"]);
    git(d, &["commit", "--quiet", "--allow-empty", "-m", "side"]);
    git(
        d,
        &[
            "notes",
            "--ref",
            "refs/notes/tak",
            "add",
            "-m",
            &line("startup", "gha", 50_000),
        ],
    );
    git(d, &["checkout", "--quiet", "main"]);
    git(
        d,
        &["merge", "--quiet", "--no-ff", "-m", "merge side", "side"],
    );
    git(
        d,
        &[
            "notes",
            "--ref",
            "refs/notes/tak",
            "add",
            "-m",
            &line("startup", "gha", 1115),
        ],
    );
    let out = tak(d, &["detect"]);
    let md = stdout(&out);
    assert!(out.status.success(), "{md}");
    assert!(!md.contains("50,000"), "the side branch leaked in: {md}");
}

/// `actions/checkout` fetches one commit by default. The walk then finds
/// nothing earlier, which on its own reads exactly like a first recording; the
/// report has to say the checkout was the reason.
#[test]
fn a_shallow_checkout_is_named_as_the_reason() {
    let (dir, _) = repo();
    let d = dir.path();
    let url = format!("file://{}", d.display());
    git(d, &["clone", "--quiet", "--depth", "2", &url, "shallow"]);
    let shallow = d.join("shallow");
    git(
        &shallow,
        &[
            "fetch",
            "--quiet",
            "origin",
            "+refs/notes/tak:refs/notes/tak",
        ],
    );
    let md = stdout(&tak(&shallow, &["detect"]));
    assert!(md.contains("This checkout is shallow"), "{md}");

    // A full clone is not warned about.
    let md = stdout(&tak(d, &["detect"]));
    assert!(!md.contains("shallow"), "{md}");
}
