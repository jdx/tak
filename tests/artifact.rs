//! The artifact boundary is a security boundary: a read-only job produces the
//! file and a different job, holding a write token, consumes it. Exercise the
//! real CLI against separate clones so target validation and note preservation
//! cannot accidentally become unit-test-only properties.

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

fn record(dir: &Path, bench: &str) {
    let out = tak(
        dir,
        &[
            "run",
            "--no-counters",
            "--runs",
            "1",
            "--warmup",
            "0",
            "--record",
            "--bench",
            bench,
            "--",
            "true",
        ],
    );
    assert!(
        out.status.success(),
        "tak run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn setup(tag: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let scratch = tempfile::Builder::new()
        .prefix(&format!("tak-artifact-{tag}-"))
        .tempdir()
        .unwrap();
    let remote = scratch.path().join("remote.git");
    git(
        scratch.path(),
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    let seed = scratch.path().join("seed");
    git(
        scratch.path(),
        &["clone", "--quiet", remote.to_str().unwrap(), "seed"],
    );
    git(
        &seed,
        &["commit", "--quiet", "--allow-empty", "-m", "parent"],
    );
    git(&seed, &["commit", "--quiet", "--allow-empty", "-m", "head"]);
    git(
        &seed,
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    );
    (scratch, remote)
}

fn clone_into(scratch: &Path, remote: &Path, name: &str) -> std::path::PathBuf {
    git(
        scratch,
        &["clone", "--quiet", remote.to_str().unwrap(), name],
    );
    scratch.join(name)
}

fn note_lines(dir: &Path, rev: &str) -> Vec<String> {
    git(
        dir,
        &[
            "fetch",
            "--quiet",
            "origin",
            "+refs/notes/tak:refs/notes/tak",
        ],
    );
    let mut lines: Vec<_> = git(dir, &["notes", "--ref", "tak", "show", rev])
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

#[test]
fn publishing_an_artifact_preserves_the_existing_note() {
    let (scratch, remote) = setup("preserve");

    let old = clone_into(scratch.path(), &remote, "old");
    record(&old, "old");
    assert!(tak(&old, &["push"]).status.success());

    let measure = clone_into(scratch.path(), &remote, "measure");
    record(&measure, "new");
    let artifact = scratch.path().join("measurement.json");
    let out = tak(
        &measure,
        &["artifact", "export", "--output", artifact.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let publish = clone_into(scratch.path(), &remote, "publish");
    let out = tak(
        &publish,
        &[
            "artifact",
            "publish",
            artifact.to_str().unwrap(),
            "--expect",
            "HEAD",
        ],
    );
    assert!(
        out.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines = note_lines(&publish, "HEAD");
    assert_eq!(lines.len(), 2, "one measurement was lost: {lines:?}");
    assert!(lines.iter().any(|line| line.contains(r#""bench":"old""#)));
    assert!(lines.iter().any(|line| line.contains(r#""bench":"new""#)));
}

#[test]
fn the_artifact_cannot_choose_a_different_commit() {
    let (scratch, remote) = setup("target");
    let measure = clone_into(scratch.path(), &remote, "measure");
    record(&measure, "new");
    let artifact = scratch.path().join("measurement.json");
    assert!(
        tak(
            &measure,
            &["artifact", "export", "--output", artifact.to_str().unwrap(),],
        )
        .status
        .success()
    );

    let publish = clone_into(scratch.path(), &remote, "publish");
    let out = tak(
        &publish,
        &[
            "artifact",
            "publish",
            artifact.to_str().unwrap(),
            "--expect",
            "HEAD^",
        ],
    );
    assert!(!out.status.success(), "mismatched target was accepted");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("but expected"),
        "unexpected error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
