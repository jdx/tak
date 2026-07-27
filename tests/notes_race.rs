//! Two writers must not be able to delete each other's measurements.
//!
//! This exists because they could. The push refspec was `+refs/notes/tak:…`,
//! and a forced push always succeeds — so the retry-and-merge loop was
//! unreachable, and a writer whose local ref was empty replaced the entire
//! remote history. In jdx/aube a CI job whose checkout had never fetched notes
//! recorded one measurement, pushed, and destroyed 60 backfilled ones.
//!
//! Nothing in the unit tests could see it: the bug lived in the interaction
//! between two clones and a remote, which is exactly the shape these tests set
//! up.

use std::path::Path;
use std::process::Command;

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

fn tak(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run tak")
}

/// Record a measurement under `bench` on HEAD, without measuring anything real.
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

/// Which benchmarks the remote holds notes for, across every annotated commit.
fn benches_on_remote(scratch: &Path, remote: &Path) -> Vec<String> {
    let probe = scratch.join("probe");
    if probe.exists() {
        std::fs::remove_dir_all(&probe).unwrap();
    }
    git(
        scratch,
        &["clone", "--quiet", remote.to_str().unwrap(), "probe"],
    );
    // Fetch may find nothing if no notes were ever pushed; that is a valid state.
    Command::new("git")
        .args(["fetch", "origin", "+refs/notes/tak:refs/notes/tak"])
        .current_dir(&probe)
        .output()
        .ok();
    let list = Command::new("git")
        .args(["notes", "--ref", "tak", "list"])
        .current_dir(&probe)
        .output()
        .unwrap();
    let mut found = Vec::new();
    for line in String::from_utf8_lossy(&list.stdout).lines() {
        let Some(target) = line.split_whitespace().nth(1) else {
            continue;
        };
        let show = Command::new("git")
            .args(["notes", "--ref", "tak", "show", target])
            .current_dir(&probe)
            .output()
            .unwrap();
        for record in String::from_utf8_lossy(&show.stdout).lines() {
            if let Some(rest) = record.split("\"bench\":\"").nth(1)
                && let Some(name) = rest.split('"').next()
            {
                found.push(name.to_string());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Set up a bare remote with one commit, and return (scratch, remote).
fn remote_with_a_commit(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let scratch = std::env::temp_dir().join(format!("tak-race-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    let remote = scratch.join("remote.git");
    git(
        &scratch,
        // -b main: a bare repo defaults HEAD to `master`, and cloning it after
        // pushing `main` leaves the clone with no HEAD to record against.
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );

    let seed = scratch.join("seed");
    git(
        &scratch,
        &["clone", "--quiet", remote.to_str().unwrap(), "seed"],
    );
    git(&seed, &["commit", "--quiet", "--allow-empty", "-m", "seed"]);
    git(
        &seed,
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    );

    (scratch, remote)
}

/// The exact shape of the aube incident: one clone pushes, a second clone that
/// never fetched notes pushes its own, and the first clone's records must
/// survive.
#[test]
fn a_second_writer_does_not_erase_the_first() {
    let (scratch, remote) = remote_with_a_commit("second");

    let a = scratch.join("a");
    git(
        &scratch,
        &["clone", "--quiet", remote.to_str().unwrap(), "a"],
    );
    record(&a, "from-a");
    assert!(tak(&a, &["push"]).status.success());

    // B is a fresh clone. Like a CI checkout, it has no notes ref at all.
    let b = scratch.join("b");
    git(
        &scratch,
        &["clone", "--quiet", remote.to_str().unwrap(), "b"],
    );
    assert!(
        !b.join(".git/refs/notes/tak").exists(),
        "a plain clone should not have the notes ref"
    );
    record(&b, "from-b");
    let out = tak(&b, &["push"]);
    assert!(
        out.status.success(),
        "second push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let found = benches_on_remote(&scratch, &remote);
    std::fs::remove_dir_all(&scratch).ok();
    assert_eq!(
        found,
        ["from-a", "from-b"],
        "the second writer erased the first"
    );
}

/// Reading must not discard local records either. `fetch` used to land straight
/// on `refs/notes/tak`, so `tak history` between recording and pushing threw the
/// measurement away — the same overwrite, one step earlier.
#[test]
fn reading_does_not_discard_unpushed_records() {
    let (scratch, remote) = remote_with_a_commit("read");

    let a = scratch.join("a");
    git(
        &scratch,
        &["clone", "--quiet", remote.to_str().unwrap(), "a"],
    );
    record(&a, "from-a");
    assert!(tak(&a, &["push"]).status.success());

    let b = scratch.join("b");
    git(
        &scratch,
        &["clone", "--quiet", remote.to_str().unwrap(), "b"],
    );
    record(&b, "from-b");
    // Reading fetches. Before the fix this replaced B's local note with A's.
    assert!(tak(&b, &["history"]).status.success());
    assert!(tak(&b, &["push"]).status.success());

    let found = benches_on_remote(&scratch, &remote);
    std::fs::remove_dir_all(&scratch).ok();
    assert_eq!(
        found,
        ["from-a", "from-b"],
        "reading threw away the unpushed record"
    );
}

/// Notes hang off commits, but `git rev-parse v1.2.3` on an *annotated* tag
/// returns the tag object. Without peeling, `tak history` and `tak compare`
/// silently find nothing on exactly the revisions people are most likely to
/// name — and "no measurements" reads as "nothing recorded", not "looked in the
/// wrong place".
#[test]
fn an_annotated_tag_resolves_to_its_commit() {
    let (scratch, remote) = remote_with_a_commit("annotated");

    let a = scratch.join("a");
    git(
        &scratch,
        &["clone", "--quiet", remote.to_str().unwrap(), "a"],
    );
    record(&a, "from-a");
    git(&a, &["tag", "-a", "v1.0.0", "-m", "release"]);

    // The tag object and the commit are different objects; that is the trap.
    let tag_object = git(&a, &["rev-parse", "v1.0.0"]);
    let commit = git(&a, &["rev-parse", "v1.0.0^{commit}"]);
    assert_ne!(tag_object, commit, "expected an annotated tag");

    let out = tak(&a, &["history", "v1.0.0"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    std::fs::remove_dir_all(&scratch).ok();
    assert!(
        stdout.contains("from-a"),
        "history on an annotated tag found nothing: {stdout}"
    );
}
