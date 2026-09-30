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
    tak_env(dir, args, &[])
}

fn tak_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(args)
        // No remote exists, so the refresh fails and falls back to local notes,
        // as it does offline. Pointing at a name keeps it from guessing.
        .args(["--remote", "nowhere", "--no-credit"])
        // The default gate and trailer policy, whatever the environment
        // running the tests says.
        .env_remove("TAK_GATE_PCT")
        .env_remove("TAK_ACCEPT_TRAILERS")
        .envs(env.iter().copied())
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

/// The gate comes from the working tree's `tak.toml`, per benchmark, exactly
/// as for `tak compare`: a report-only benchmark's 10% step is reported and
/// does not fail, and a loosened gate lets it through as a pass.
#[test]
fn a_benchmark_gate_in_tak_toml_applies() {
    let (dir, c) = repo();
    let toml = dir.path().join("tak.toml");

    std::fs::write(
        &toml,
        "[bench.startup]\ncmd = [\"true\"]\ngate = { enabled = false }\n",
    )
    .unwrap();
    let out = tak(dir.path(), &["detect", &c[3]]);
    let md = stdout(&out);
    assert!(out.status.success(), "report-only never fails: {md}");
    assert!(md.contains("**+10.00%** (not gated)"), "{md}");
    assert!(md.contains("Every benchmark here is report-only"), "{md}");

    std::fs::write(
        &toml,
        "[bench.startup]\ncmd = [\"true\"]\ngate = { pct = 20.0 }\n",
    )
    .unwrap();
    let out = tak(dir.path(), &["detect", &c[3]]);
    let md = stdout(&out);
    assert!(out.status.success(), "10% is within a 20% gate: {md}");
    assert!(md.contains("| 20% |"), "{md}");

    std::fs::write(
        &toml,
        "[bench.startup]\ncmd = [\"true\"]\ngate = { pct = 5.0 }\n",
    )
    .unwrap();
    let out = tak(dir.path(), &["detect", &c[3]]);
    assert!(!out.status.success(), "10% is past a 5% gate");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stepped beyond their gate"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

/// A head with nothing recorded compares nothing. That fails, and says why,
/// rather than reading like a pass.
#[test]
fn an_unrecorded_head_fails_as_nothing_compared() {
    let (dir, c) = repo();
    let out = tak(dir.path(), &["detect", &c[2]]);
    let md = stdout(&out);
    assert!(!out.status.success(), "{md}");
    assert!(
        md.contains("**Nothing was compared at `") && md.contains("so this check fails.**"),
        "{md}"
    );
    assert!(md.contains("No instruction counts are recorded"), "{md}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("pass --allow-empty"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The first recording has nothing before it. It fails by default and passes
/// with `--allow-empty` or with `--no-gate`, which never fails.
#[test]
fn a_first_recording_needs_allow_empty() {
    let (dir, c) = repo();
    let out = tak(dir.path(), &["detect", &c[0]]);
    let md = stdout(&out);
    assert!(!out.status.success(), "{md}");
    assert!(
        md.contains("has an earlier recorded point in the window"),
        "{md}"
    );
    assert!(md.contains("pass `--allow-empty`"), "{md}");

    let out = tak(dir.path(), &["detect", &c[0], "--no-gate"]);
    let md = stdout(&out);
    assert!(out.status.success(), "--no-gate never fails: {md}");
    assert!(md.contains("nothing was gated** (`--no-gate`)"), "{md}");

    let out = tak(dir.path(), &["detect", &c[0], "--allow-empty"]);
    let md = stdout(&out);
    assert!(out.status.success(), "{md}");
    assert!(md.contains("nothing was gated** (`--allow-empty`)"), "{md}");

    // It waives only the empty case: a real step still fails.
    let out = tak(dir.path(), &["detect", &c[3], "--allow-empty"]);
    assert!(!out.status.success(), "{}", stdout(&out));
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

/// tak refreshes notes with a depth-1 fetch, and git records that as a
/// shallow boundary *in the notes history* — `.git/shallow` appears and
/// `--is-shallow-repository` says true in what is a full clone of the project.
/// A walk that reached the real root must not blame a shallow checkout.
#[test]
fn a_shallow_notes_fetch_is_not_a_shallow_checkout() {
    let (dir, _) = repo();
    let d = dir.path();
    // A second notes commit, so the depth-1 fetch has a parent to cut off.
    git(
        d,
        &[
            "notes",
            "--ref",
            "refs/notes/tak",
            "append",
            "-m",
            &line("other", "gha", 1),
            "HEAD~1",
        ],
    );
    let url = format!("file://{}", d.display());
    git(d, &["clone", "--quiet", &url, "full"]);
    let full = d.join("full");
    git(
        &full,
        &[
            "fetch",
            "--quiet",
            "--depth",
            "1",
            "origin",
            "+refs/notes/tak:refs/notes/tak",
        ],
    );
    assert_eq!(
        git(&full, &["rev-parse", "--is-shallow-repository"]),
        "true",
        "the premise: git now calls this clone shallow"
    );

    let md = stdout(&tak(&full, &["detect"]));
    assert!(md.contains("Sustained drift"), "the walk ran: {md}");
    assert!(!md.contains("This checkout is shallow"), "{md}");
}

/// A single first-parent line of `total` empty commits, with `startup` on
/// `gha` recorded at the root and at the tip.
///
/// fast-import builds them in one process; ten thousand `git commit` calls
/// would not.
fn linear_history(prefix: &str, total: usize, root: u64, tip: u64) -> tempfile::TempDir {
    use std::io::Write;

    let dir = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
    let d = dir.path();
    git(d, &["init", "--quiet", "-b", "main"]);
    let mut stream = String::new();
    for i in 1..=total {
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{i}\ncommitter t <t@example.com> {i} +0000\ndata 2\nc\n"
        ));
        if i > 1 {
            stream.push_str(&format!("from :{}\n", i - 1));
        }
        stream.push('\n');
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(d)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "fast-import failed");
    git(d, &["reset", "--quiet", "--hard", "main"]);

    let first = git(d, &["rev-list", "--max-parents=0", "HEAD"]);
    for (rev, value) in [(first.as_str(), root), ("HEAD", tip)] {
        git(
            d,
            &[
                "notes",
                "--ref",
                "refs/notes/tak",
                "add",
                "-m",
                &line("startup", "gha", value),
                rev,
            ],
        );
    }
    dir
}

/// A previous recording further back than the scan limit is never reached.
/// The walk must say it stopped at the limit rather than report the head's
/// series as brand new, and the empty comparison must still fail. One commit
/// past the limit is the tightest case: the root is the only one left out.
#[test]
fn hitting_the_scan_limit_is_named_as_the_reason() {
    let total = tak_cli::detect::SCAN_LIMIT + 1;
    let dir = linear_history("tak-detect-over-", total, 1000, 2000);

    let out = tak(dir.path(), &["detect"]);
    let md = stdout(&out);
    assert!(!out.status.success(), "nothing was compared: {md}");
    assert!(md.contains("limit of 10,000 first-parent commits"), "{md}");
    assert!(!md.contains("This checkout is shallow"), "{md}");
}

/// A history of exactly the limit is complete: the walk reaches the root, the
/// root's recording is compared, and nothing claims older ones were missed.
#[test]
fn a_history_of_exactly_the_limit_is_not_cut_short() {
    let total = tak_cli::detect::SCAN_LIMIT;
    let dir = linear_history("tak-detect-exact-", total, 1000, 1000);

    let out = tak(dir.path(), &["detect"]);
    let md = stdout(&out);
    assert!(out.status.success(), "{md}");
    assert!(
        md.contains(&format!("Walked {total} first-parent commit(s)")),
        "{md}"
    );
    assert!(!md.contains("limit of"), "{md}");
    assert!(!md.contains("Nothing was compared"), "{md}");
}

/// c0 recorded at 1000, c1 unrecorded with a `Tak-Accept: startup` trailer,
/// c2 recorded at 1100: a 10% step whose range, c0..c2, holds the trailer.
fn accepted_repo() -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::Builder::new()
        .prefix("tak-detect-accept-")
        .tempdir()
        .unwrap();
    let d = dir.path();
    git(d, &["init", "--quiet", "-b", "main"]);
    let mut shas = Vec::new();
    for (msg, value) in [
        ("c0", Some(1000)),
        ("c1\n\nTak-Accept: startup", None),
        ("c2", Some(1100)),
    ] {
        git(d, &["commit", "--quiet", "--allow-empty", "-m", msg]);
        if let Some(v) = value {
            git(
                d,
                &[
                    "notes",
                    "--ref",
                    "refs/notes/tak",
                    "add",
                    "-m",
                    &line("startup", "gha", v),
                ],
            );
        }
        shas.push(git(d, &["rev-parse", "HEAD"]));
    }
    (dir, shas)
}

/// Trailers are ignored unless `accept_trailers` is on, and the report says
/// one was found. On, the trailer in the step's range accepts it.
#[test]
fn a_trailer_in_the_steps_range_accepts_it_when_enabled() {
    let (dir, c) = accepted_repo();

    let out = tak(dir.path(), &["detect"]);
    let md = stdout(&out);
    assert!(!out.status.success(), "trailers are off by default: {md}");
    assert!(md.contains("were found but not honoured"), "{md}");

    let out = tak_env(dir.path(), &["detect"], &[("TAK_ACCEPT_TRAILERS", "1")]);
    let md = stdout(&out);
    assert!(out.status.success(), "{md}");
    assert!(md.contains("**+10.00%** (accepted)"), "{md}");
    assert!(
        md.contains(&format!("(accepted by `Tak-Accept` in `{}`)", short(&c[1]))),
        "{md}"
    );
}

/// `--accept` works whatever the setting says: the escape for a manual rerun
/// of a main-branch job whose step was accepted on its pull request.
#[test]
fn accept_flag_waives_a_step_onto_the_head() {
    let (dir, _) = accepted_repo();
    let out = tak(dir.path(), &["detect", "--accept", "startup"]);
    let md = stdout(&out);
    assert!(out.status.success(), "{md}");
    assert!(md.contains("(accepted by `--accept`)"), "{md}");

    // Exact names only: another benchmark's acceptance changes nothing.
    let out = tak(dir.path(), &["detect", "--accept", "other"]);
    assert!(!out.status.success());
    // And an empty one is an error before anything is walked.
    let out = tak(dir.path(), &["detect", "--accept", ""]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("empty"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The range is first-parent. A trailer on a branch commit behind a merge's
/// second parent did not land on main as its own commit, so it does not
/// accept the step.
#[test]
fn only_first_parent_trailers_in_the_range_count() {
    let (dir, _) = repo();
    let d = dir.path();
    // A side branch whose commit carries the trailer, merged with a plain
    // merge commit that is then recorded well above c7.
    git(d, &["checkout", "--quiet", "-b", "side"]);
    git(
        d,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "side\n\nTak-Accept: startup",
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
            &line("startup", "gha", 1300),
        ],
    );
    let out = tak_env(d, &["detect"], &[("TAK_ACCEPT_TRAILERS", "1")]);
    let md = stdout(&out);
    assert!(
        !out.status.success(),
        "the side commit is not first-parent: {md}"
    );
    assert!(!md.contains("(accepted)"), "{md}");
}
