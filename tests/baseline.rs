//! Local baselines, through the real binary in a scratch repository.
//!
//! The loop under test is the one a developer runs by hand: save, change the
//! command, measure again, read the report. What matters is observable from
//! outside — where the file lands, what is printed, the exit status, and that
//! nothing reaches `refs/notes/tak` — so that is what these check.

#![cfg(unix)]

use std::path::{Path, PathBuf};
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

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("tak-baseline-")
            .tempdir()
            .unwrap();
        let dir = tmp.path().join("repo");
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        Repo { _tmp: tmp, dir }
    }

    fn baseline_file(&self, name: &str) -> PathBuf {
        self.dir
            .join(".git/tak/baselines")
            .join(format!("{name}.jsonl"))
    }
}

/// `tak run` in `dir`, quick and quiet: one warmup-free pass of a few runs, no
/// counters unless a test asks for them, and a fixed runner class so a CI host
/// and a laptop produce the same series names. An empty `cmd` runs what the
/// directory's tak.toml declares.
fn tak_run(dir: &Path, flags: &[&str], cmd: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tak"));
    c.args(["run", "--no-progress", "--runs", "3", "--warmup", "0"])
        .args(["--runner", "test-runner"])
        .args(flags);
    if !cmd.is_empty() {
        c.arg("--").args(cmd);
    }
    c.current_dir(dir).output().expect("failed to run tak")
}

/// Write a baseline by hand, for states a host without valgrind cannot
/// produce by measuring: an instruction count on the baseline side.
fn write_baseline(repo: &Repo, name: &str, lines: &[&str]) {
    let path = repo.baseline_file(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn counted_line(bench: &str) -> String {
    format!(
        r#"{{"v":1,"bench":"{bench}","tool":"self","runner":"test-runner","ts":"2026-09-29T00:00:00Z","metrics":{{"instructions":1000000.0,"wall_min_ms":1.0}}}}"#
    )
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) -> &Output {
    assert!(
        o.status.success(),
        "tak failed\nstdout:\n{}\nstderr:\n{}",
        stdout(o),
        stderr(o)
    );
    o
}

fn fail(o: &Output) -> String {
    assert!(
        !o.status.success(),
        "tak should have failed\nstdout:\n{}",
        stdout(o)
    );
    stderr(o)
}

/// Nothing a baseline does may touch the notes ref: it would attach a
/// measurement of an uncommitted tree to a commit, and `tak push` would
/// publish it.
fn assert_no_notes(dir: &Path) {
    assert_eq!(git(dir, &["for-each-ref", "refs/notes"]), "");
}

const NC: &str = "--no-counters";

#[test]
fn a_saved_baseline_is_compared_against_without_writing_notes() {
    let repo = Repo::new();
    let out = tak_run(
        &repo.dir,
        &[NC, "--bench", "startup", "--save-baseline", "before"],
        &["true"],
    );
    assert!(stdout(ok(&out)).contains("saved 1 measurement(s) to baseline `before`"));
    let saved = std::fs::read_to_string(repo.baseline_file("before")).unwrap();
    assert!(saved.contains(r#""bench":"startup""#), "{saved}");
    assert!(saved.contains(r#""runner":"test-runner""#), "{saved}");

    // The edit: same benchmark, different work.
    let out = tak_run(
        &repo.dir,
        &[NC, "--bench", "startup", "--baseline", "before"],
        &["sleep", "0.02"],
    );
    let report = stdout(ok(&out));
    assert!(
        report.contains("compared against baseline `before`"),
        "{report}"
    );
    assert!(report.contains("| startup |"), "{report}");
    // The wall-clock column moved from `true` to a 20ms sleep.
    assert!(report.contains("ms | +"), "{report}");
    assert!(report.contains("Only instruction counts gate"), "{report}");
    assert!(!report.contains("Measured by [tak]"), "{report}");

    assert_no_notes(&repo.dir);
    // Comparing is read-only; the baseline is exactly what was saved.
    assert_eq!(
        std::fs::read_to_string(repo.baseline_file("before")).unwrap(),
        saved
    );
}

/// A mistyped name has to cost a second, not a full run and then an error.
#[test]
fn a_missing_baseline_fails_before_anything_is_measured() {
    let repo = Repo::new();
    ok(&tak_run(
        &repo.dir,
        &[NC, "--save-baseline", "main"],
        &["true"],
    ));
    let marker = repo.dir.join("ran");
    let err = fail(&tak_run(
        &repo.dir,
        &[NC, "--baseline", "mian"],
        &["touch", marker.to_str().unwrap()],
    ));
    assert!(err.contains("no baseline `mian` (saved: main)"), "{err}");
    assert!(
        !marker.exists(),
        "the command ran before the name was checked"
    );
}

#[test]
fn a_name_that_is_not_a_file_name_is_rejected() {
    let repo = Repo::new();
    let err = fail(&tak_run(
        &repo.dir,
        &[NC, "--save-baseline", "../escape"],
        &["true"],
    ));
    assert!(err.contains("invalid baseline name"), "{err}");
    assert!(!repo.dir.join(".git/tak/escape.jsonl").exists());
}

/// No fallback location outside a repository: anywhere else is shared between
/// unrelated projects or may be committed.
#[test]
fn baselines_need_a_git_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(["run", NC, "--runs", "1", "--warmup", "0"])
        .args(["--save-baseline", "x", "--", "true"])
        .current_dir(tmp.path())
        // Stop git from finding a repository above the scratch directory.
        .env("GIT_CEILING_DIRECTORIES", tmp.path().parent().unwrap())
        .output()
        .unwrap();
    let err = fail(&out);
    assert!(err.contains("need a git repository"), "{err}");
}

#[test]
fn gate_without_a_baseline_is_an_error() {
    let repo = Repo::new();
    let err = fail(&tak_run(&repo.dir, &[NC, "--gate"], &["true"]));
    assert!(err.contains("--gate applies to --baseline"), "{err}");
}

/// A baseline taken on another runner class must not be compared, and must
/// not look like a clean comparison either.
#[test]
fn a_baseline_from_another_runner_is_named_and_not_compared() {
    let repo = Repo::new();
    ok(&tak_run(
        &repo.dir,
        &[NC, "--runner", "laptop", "--save-baseline", "before"],
        &["true"],
    ));
    // Report only: says so, exits 0.
    let out = tak_run(
        &repo.dir,
        &[NC, "--runner", "ci", "--baseline", "before"],
        &["true"],
    );
    let (report, warn) = (stdout(ok(&out)), stderr(&out));
    assert!(report.contains("Nothing was compared"), "{report}");
    assert!(
        warn.contains("runner class `laptop`, and this run is on `ci`"),
        "{warn}"
    );
    // Gated: comparing nothing is a failure, not a pass.
    let err = fail(&tak_run(
        &repo.dir,
        &[NC, "--runner", "ci", "--baseline", "before", "--gate"],
        &["true"],
    ));
    assert!(err.contains("nothing to gate"), "{err}");
}

/// Without instruction counts there is nothing a gate may fire on, and a gate
/// that passed anyway would pass every edit.
#[test]
fn a_gate_with_only_wall_clock_fails_rather_than_passing() {
    let repo = Repo::new();
    ok(&tak_run(
        &repo.dir,
        &[NC, "--save-baseline", "b"],
        &["true"],
    ));
    let err = fail(&tak_run(
        &repo.dir,
        &[NC, "--baseline", "b", "--gate"],
        &["true"],
    ));
    assert!(err.contains("nothing to gate"), "{err}");
}

/// Running one benchmark against a baseline saved by several must not report
/// the others as vanished, and saving it must keep them.
#[test]
fn a_subset_run_neither_reports_nor_discards_the_rest() {
    let repo = Repo::new();
    ok(&tak_run(
        &repo.dir,
        &[NC, "--bench", "a", "--save-baseline", "x"],
        &["true"],
    ));
    ok(&tak_run(
        &repo.dir,
        &[NC, "--bench", "b", "--save-baseline", "x"],
        &["true"],
    ));
    let saved = std::fs::read_to_string(repo.baseline_file("x")).unwrap();
    assert_eq!(saved.lines().count(), 2, "{saved}");

    let out = tak_run(
        &repo.dir,
        &[NC, "--bench", "a", "--baseline", "x"],
        &["true"],
    );
    let report = stdout(ok(&out));
    assert!(report.contains("| a |"), "{report}");
    assert!(!report.contains("`b`"), "b was not run: {report}");
}

/// `--record` and `--save-baseline` together write both; the baseline flags
/// alone never write notes (checked in the first test).
#[test]
fn record_and_save_baseline_combine() {
    let repo = Repo::new();
    git(&repo.dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let out = tak_run(
        &repo.dir,
        &[NC, "--bench", "s", "--record", "--save-baseline", "x"],
        &["true"],
    );
    let text = stdout(ok(&out));
    assert!(text.contains("recorded 1 measurement(s)"), "{text}");
    assert!(repo.baseline_file("x").exists());
    assert!(
        git(
            &repo.dir,
            &["notes", "--ref", "refs/notes/tak", "show", "HEAD"]
        )
        .contains("\"s\"")
    );
}

/// Baselines live in the common git directory, so a second worktree sees the
/// first's — which is how two branches checked out side by side compare.
#[test]
fn worktrees_share_baselines() {
    let repo = Repo::new();
    git(&repo.dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let wt = repo.dir.parent().unwrap().join("wt");
    git(
        &repo.dir,
        &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
    );
    ok(&tak_run(
        &repo.dir,
        &[NC, "--save-baseline", "main"],
        &["true"],
    ));
    let out = tak_run(&wt, &[NC, "--baseline", "main"], &["true"]);
    assert!(stdout(ok(&out)).contains("compared against baseline `main`"));
}

/// A benchmark counted in the baseline but not in this run — its count
/// failed, or counters were turned off — has not been checked, so a gate
/// that passed would be vouching for it anyway.
#[test]
fn a_gate_fails_when_a_counted_benchmark_went_uncounted() {
    let repo = Repo::new();
    write_baseline(&repo, "x", &[&counted_line("a"), &counted_line("b")]);
    let out = tak_run(
        &repo.dir,
        &[NC, "--bench", "a", "--baseline", "x", "--gate"],
        &["true"],
    );
    let err = fail(&out);
    assert!(
        err.contains("one side only for `a` on `test-runner`"),
        "{err}"
    );
    assert!(
        stdout(&out).contains("Counted on one side only, so not gated: `a`"),
        "{}",
        stdout(&out)
    );
    // Report only: said, not failed.
    ok(&tak_run(
        &repo.dir,
        &[NC, "--bench", "a", "--baseline", "x"],
        &["true"],
    ));
}

/// A gate that measured nothing has checked nothing. `git bisect run` would
/// otherwise mark the revision good.
#[test]
fn a_gate_with_nothing_to_run_fails() {
    let repo = Repo::new();
    write_baseline(&repo, "x", &[&counted_line("off")]);

    std::fs::write(
        repo.dir.join("tak.toml"),
        "[bench.off]\ncmd = \"true\"\nwhen = \"false\"\n",
    )
    .unwrap();
    let err = fail(&tak_run(&repo.dir, &["--baseline", "x", "--gate"], &[]));
    assert!(
        err.contains("nothing to gate against baseline `x`"),
        "{err}"
    );
    assert!(err.contains("false `when`"), "{err}");
    ok(&tak_run(&repo.dir, &["--baseline", "x"], &[]));

    std::fs::write(repo.dir.join("tak.toml"), "").unwrap();
    let err = fail(&tak_run(&repo.dir, &["--baseline", "x", "--gate"], &[]));
    assert!(err.contains("declares no benchmarks"), "{err}");
}

/// A subject whose check fails usually got faster by skipping its work. The
/// report has to say so beside the table, and a gate must not pass it.
#[test]
fn a_failed_check_is_flagged_and_fails_the_gate() {
    let repo = Repo::new();
    write_baseline(&repo, "x", &[&counted_line("c")]);
    std::fs::write(
        repo.dir.join("tak.toml"),
        "[bench.c]\ncmd = \"true\"\ncheck = \"false\"\n",
    )
    .unwrap();

    let out = tak_run(&repo.dir, &[NC, "--baseline", "x"], &[]);
    let report = stdout(ok(&out));
    assert!(
        report.contains(
            "Check failed, so this comparison is not evidence of an improvement:** c failed 3 of 3"
        ),
        "{report}"
    );

    let err = fail(&tak_run(&repo.dir, &[NC, "--baseline", "x", "--gate"], &[]));
    assert!(err.contains("check failed, so nothing is gated"), "{err}");
}

/// The baseline is written before the notes, and a failed recording says the
/// baseline was saved, so a retry of the same command is the whole fix.
#[test]
fn a_failed_record_after_a_saved_baseline_says_which_was_written() {
    // No commit: there is no HEAD to record against.
    let repo = Repo::new();
    let err = fail(&tak_run(
        &repo.dir,
        &[NC, "--record", "--save-baseline", "x"],
        &["true"],
    ));
    assert!(
        err.contains("baseline `x` was saved, but nothing was recorded"),
        "{err}"
    );
    assert!(repo.baseline_file("x").exists());
    assert_no_notes(&repo.dir);
}

/// Several processes saving different benchmarks to one baseline at once, as
/// parallel worktrees do, keep every benchmark.
#[test]
fn concurrent_saves_from_separate_processes_keep_every_benchmark() {
    let repo = Repo::new();
    let children: Vec<_> = (0..6)
        .map(|i| {
            Command::new(env!("CARGO_BIN_EXE_tak"))
                .args(["run", NC, "--no-progress", "--runs", "1", "--warmup", "0"])
                .args(["--bench", &format!("b{i}"), "--save-baseline", "x"])
                .args(["--", "true"])
                .current_dir(&repo.dir)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    let saved = std::fs::read_to_string(repo.baseline_file("x")).unwrap();
    assert_eq!(saved.lines().count(), 6, "{saved}");
}

/// The gate itself, on real instruction counts. Skips without valgrind, the
/// normal state on macOS and Windows.
#[test]
fn the_gate_fires_on_an_instruction_count_regression() {
    if !tak_cli::measure::valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let repo = Repo::new();
    let quick = ["sh", "-c", "exit 0"];
    // Enough shell arithmetic to retire far more than 1% extra instructions.
    let slow = [
        "sh",
        "-c",
        "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done",
    ];
    ok(&tak_run(
        &repo.dir,
        &["--bench", "loop", "--save-baseline", "b"],
        &quick,
    ));

    // Same work: the gate passes.
    let out = tak_run(
        &repo.dir,
        &["--bench", "loop", "--baseline", "b", "--gate"],
        &quick,
    );
    assert!(stdout(ok(&out)).contains("No instruction-count regression"));

    // More work: reported, and failed.
    let out = tak_run(
        &repo.dir,
        &["--bench", "loop", "--baseline", "b", "--gate"],
        &slow,
    );
    let err = fail(&out);
    assert!(err.contains("regressed by more than"), "{err}");
    assert!(
        stdout(&out).contains("above the 1% gate"),
        "{}",
        stdout(&out)
    );

    // Without --gate the same regression is reported and exits 0.
    ok(&tak_run(
        &repo.dir,
        &["--bench", "loop", "--baseline", "b"],
        &slow,
    ));
    assert_no_notes(&repo.dir);
}

/// The partial case on real counts: `a` compares cleanly, `b` was counted in
/// the baseline and not now. Passing on `a` alone would leave `b` unchecked.
#[test]
fn a_gate_that_could_check_only_some_benchmarks_fails() {
    if !tak_cli::measure::valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let repo = Repo::new();
    let toml = |b_counters: bool| {
        format!(
            "[bench.m.subject.a]\ncmd = [\"sh\", \"-c\", \"exit 0\"]\ncounters = true\n\n\
             [bench.m.subject.b]\ncmd = [\"sh\", \"-c\", \"exit 0\"]\ncounters = {b_counters}\n"
        )
    };
    std::fs::write(repo.dir.join("tak.toml"), toml(true)).unwrap();
    ok(&tak_run(&repo.dir, &["--save-baseline", "x"], &[]));
    ok(&tak_run(&repo.dir, &["--baseline", "x", "--gate"], &[]));

    std::fs::write(repo.dir.join("tak.toml"), toml(false)).unwrap();
    let err = fail(&tak_run(&repo.dir, &["--baseline", "x", "--gate"], &[]));
    assert!(
        err.contains("one side only for `m` (b) on `test-runner`"),
        "{err}"
    );
}
