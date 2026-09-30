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
        warn.contains("holds `default` on `ci` only for runner class `laptop`"),
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
        err.contains("`a` on `test-runner` (not counted in this run)"),
        "{err}"
    );
    assert!(
        stdout(&out).contains("Not gated: `a` on `test-runner` (not counted in this run)"),
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

    // Saving over `b` on a failed gate keeps `b`, so a retry fails again
    // rather than comparing the regression with itself.
    let replace = [
        "--bench",
        "loop",
        "--baseline",
        "b",
        "--save-baseline",
        "b",
        "--gate",
    ];
    for _ in 0..2 {
        let err = fail(&tak_run(&repo.dir, &replace, &slow));
        assert!(err.contains("regressed by more than"), "{err}");
        assert!(err.contains("baseline `b` was not replaced"), "{err}");
    }
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
        err.contains("`m` (b) on `test-runner` (not counted in this run)"),
        "{err}"
    );
}

/// A baseline saved on two runner classes gates on either one against its
/// own series, and fails on a third with the classes it does hold named.
#[test]
fn a_baseline_saved_on_two_runners_gates_on_each() {
    if !tak_cli::measure::valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let repo = Repo::new();
    let cmd = ["sh", "-c", "exit 0"];
    for runner in ["r1", "r2"] {
        ok(&tak_run(
            &repo.dir,
            &["--runner", runner, "--bench", "s", "--save-baseline", "x"],
            &cmd,
        ));
    }
    for runner in ["r1", "r2"] {
        let out = tak_run(
            &repo.dir,
            &[
                "--runner",
                runner,
                "--bench",
                "s",
                "--baseline",
                "x",
                "--gate",
            ],
            &cmd,
        );
        let report = stdout(ok(&out));
        assert!(
            report.contains("No instruction-count regression"),
            "{report}"
        );
        assert!(
            !stderr(&out).contains("only for runner class"),
            "{}",
            stderr(&out)
        );
    }
    let err = fail(&tak_run(
        &repo.dir,
        &[
            "--runner",
            "r3",
            "--bench",
            "s",
            "--baseline",
            "x",
            "--gate",
        ],
        &cmd,
    ));
    assert!(
        err.contains("`s` on `r3` (saved only on runner class `r1`, `r2`)"),
        "{err}"
    );
}

/// The same, without valgrind: a hand-written baseline on two classes, and a
/// run on a third that counts nothing, is reported as another class's data
/// and not compared.
#[test]
fn a_baseline_on_two_other_runners_is_named_and_not_compared() {
    let repo = Repo::new();
    let line = |runner: &str| counted_line("s").replace("test-runner", runner);
    write_baseline(&repo, "x", &[&line("r1"), &line("r2")]);
    let out = tak_run(
        &repo.dir,
        &[NC, "--runner", "r3", "--bench", "s", "--baseline", "x"],
        &["true"],
    );
    ok(&out);
    assert!(
        stderr(&out).contains("holds `s` on `r3` only for runner class `r1`, `r2`"),
        "{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("Nothing was compared"),
        "{}",
        stdout(&out)
    );
}

/// An ad-hoc `tak run -- CMD` never depended on the declared benchmarks, and
/// `--baseline` must not make it: a broken, unrelated `[bench.x]` does not
/// stop the report. `--gate` is different: its verdict needs the real gates.
#[test]
fn an_invalid_unrelated_benchmark_does_not_block_an_ad_hoc_baseline_run() {
    let repo = Repo::new();
    std::fs::write(
        repo.dir.join("tak.toml"),
        "[bench.broken]\nruns = \"lots\"\n",
    )
    .unwrap();
    // The premise: a plain ad-hoc run ignores the broken benchmark.
    ok(&tak_run(&repo.dir, &[NC, "--bench", "s"], &["true"]));

    ok(&tak_run(
        &repo.dir,
        &[NC, "--bench", "s", "--save-baseline", "x"],
        &["true"],
    ));
    let out = tak_run(
        &repo.dir,
        &[NC, "--bench", "s", "--baseline", "x"],
        &["true"],
    );
    assert!(stdout(ok(&out)).contains("compared against baseline `x`"));
    assert!(
        !stderr(&out).contains("per-benchmark gates"),
        "{}",
        stderr(&out)
    );

    // Gated: the verdict needs the file's gates, so a file that will not
    // load fails the run up front — the command never runs — rather than
    // gating against a threshold the project did not choose.
    let marker = repo.dir.join("ran");
    let out = tak_run(
        &repo.dir,
        &[NC, "--bench", "s", "--baseline", "x", "--gate"],
        &["touch", marker.to_str().unwrap()],
    );
    let err = fail(&out);
    assert!(
        err.contains("--gate needs a valid tak.toml to find per-benchmark gates"),
        "{err}"
    );
    assert!(err.contains("broken"), "the config error is shown: {err}");
    assert!(!err.contains("nothing to gate"), "{err}");
    assert!(
        !marker.exists(),
        "the command ran before the file was checked"
    );
}

/// Saving over the baseline a run was gated against, when the gate fails,
/// would make a retry compare the failure with itself and pass. So the
/// baseline is kept, and the error says so. Another name is saved as usual.
#[test]
fn a_failed_gate_does_not_replace_the_baseline_it_was_gated_against() {
    let repo = Repo::new();
    write_baseline(&repo, "good", &[&counted_line("s")]);
    let before = std::fs::read_to_string(repo.baseline_file("good")).unwrap();

    // No counters here, so the gate fails: `s` is not counted in this run.
    let flags = [NC, "--bench", "s", "--baseline", "good", "--gate"];
    let err = fail(&tak_run(
        &repo.dir,
        &[&flags[..], &["--save-baseline", "good"]].concat(),
        &["true"],
    ));
    assert!(
        err.contains("baseline `good` was not replaced because the gate failed"),
        "{err}"
    );
    assert!(err.contains("not counted in this run"), "{err}");
    assert_eq!(
        std::fs::read_to_string(repo.baseline_file("good")).unwrap(),
        before
    );

    let err = fail(&tak_run(
        &repo.dir,
        &[&flags[..], &["--save-baseline", "other"]].concat(),
        &["true"],
    ));
    assert!(!err.contains("was not replaced"), "{err}");
    assert!(repo.baseline_file("other").exists());
}

/// A shell loop of `n` iterations: about 11,500 instructions each, so 2000
/// against 2500 is a rise of roughly 25% and 5.8M instructions.
fn shell_loop(n: u32) -> String {
    format!(r#"["sh", "-c", "i=0; while [ $i -lt {n} ]; do i=$((i+1)); done"]"#)
}

/// `--baseline --gate` holds each benchmark to its own gate, as `tak compare`
/// does: a loosened `pct` absorbs a rise, a report-only benchmark never
/// fails, and the global `min_delta` floor applies.
#[test]
fn a_baseline_gate_holds_each_benchmark_to_its_own_gate() {
    if !tak_cli::measure::valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let repo = Repo::new();
    let toml = |strict: u32, other: u32| {
        format!(
            "[bench.strict]\ncmd = {}\n\n\
             [bench.loose]\ncmd = {}\ngate = {{ pct = 50.0 }}\n\n\
             [bench.watch]\ncmd = {}\ngate = {{ enabled = false }}\n",
            shell_loop(strict),
            shell_loop(other),
            shell_loop(other)
        )
    };
    let write =
        |strict, other| std::fs::write(repo.dir.join("tak.toml"), toml(strict, other)).unwrap();

    write(2000, 2000);
    ok(&tak_run(&repo.dir, &["--save-baseline", "x"], &[]));

    // `loose` and `watch` rise ~25%: within 50%, and report only.
    write(2000, 2500);
    let out = tak_run(&repo.dir, &["--baseline", "x", "--gate"], &[]);
    let report = stdout(ok(&out));
    assert!(report.contains("report only"), "{report}");

    // `strict` rises too, past the global 1%.
    write(2500, 2500);
    let err = fail(&tak_run(&repo.dir, &["--baseline", "x", "--gate"], &[]));
    assert!(
        err.contains("1 benchmark(s) regressed beyond their gate against baseline `x`"),
        "{err}"
    );

    // A floor above the rise lets it through.
    ok(&tak_run(
        &repo.dir,
        &["--baseline", "x", "--gate", "--gate-min-delta", "100000000"],
        &[],
    ));
}

/// A subject this run did not count, whose count the baseline holds only for
/// another runner class, must not be skipped while its neighbour passes.
#[test]
fn an_uncounted_subject_counted_only_on_another_runner_fails_the_gate() {
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
    // Both subjects on `r1`; only `a` on `r2`.
    ok(&tak_run(
        &repo.dir,
        &["--runner", "r1", "--save-baseline", "x"],
        &[],
    ));
    ok(&tak_run(
        &repo.dir,
        &["--runner", "r2", "--subject", "a", "--save-baseline", "x"],
        &[],
    ));

    std::fs::write(repo.dir.join("tak.toml"), toml(false)).unwrap();
    let err = fail(&tak_run(
        &repo.dir,
        &["--runner", "r2", "--baseline", "x", "--gate"],
        &[],
    ));
    assert!(
        err.contains(
            "`m` (b) on `r2` (not counted in this run, and counted in the baseline only on \
             runner class `r1`)"
        ),
        "{err}"
    );
}

/// The same gap, reported without valgrind: nothing is counted here, and the
/// baseline counted the benchmark only on another class.
#[test]
fn an_uncounted_benchmark_counted_only_elsewhere_is_reported() {
    let repo = Repo::new();
    write_baseline(
        &repo,
        "x",
        &[&counted_line("s").replace("test-runner", "r1")],
    );
    let out = tak_run(
        &repo.dir,
        &[NC, "--runner", "r2", "--bench", "s", "--baseline", "x"],
        &["true"],
    );
    assert!(
        stdout(ok(&out)).contains(
            "Not gated: `s` on `r2` (not counted in this run, and counted in the baseline \
             only on runner class `r1`)"
        ),
        "{}",
        stdout(&out)
    );
}

/// A report-only benchmark's failed check is still flagged, but does not fail
/// the gate on its own; a gated one's does.
#[test]
fn a_report_only_failed_check_is_flagged_not_failed() {
    if !tak_cli::measure::valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let repo = Repo::new();
    let toml = |check: &str| {
        format!(
            "[bench.gated]\ncmd = [\"sh\", \"-c\", \"exit 0\"]\n\n\
             [bench.watch]\ncmd = [\"sh\", \"-c\", \"exit 0\"]\ncheck = \"{check}\"\n\
             gate = {{ enabled = false }}\n"
        )
    };
    std::fs::write(repo.dir.join("tak.toml"), toml("true")).unwrap();
    ok(&tak_run(&repo.dir, &["--save-baseline", "x"], &[]));

    std::fs::write(repo.dir.join("tak.toml"), toml("false")).unwrap();
    let out = tak_run(&repo.dir, &["--baseline", "x", "--gate"], &[]);
    assert!(
        stdout(ok(&out)).contains("Check failed"),
        "{}",
        stdout(&out)
    );
}

/// A failed metric stops a baseline being saved as it stops `--record`: a
/// baseline missing a declared metric would be compared against as if the
/// metric had been removed. An existing baseline of that name is left alone.
#[test]
fn a_failing_metric_saves_no_baseline() {
    let repo = Repo::new();
    std::fs::write(
        repo.dir.join("tak.toml"),
        "[bench.startup]\ncmd = \"true\"\n[bench.startup.metric.binary_bytes]\nfile = \"bin\"\n",
    )
    .unwrap();
    std::fs::write(repo.dir.join("bin"), vec![0u8; 100]).unwrap();
    ok(&tak_run(&repo.dir, &[NC, "--save-baseline", "before"], &[]));
    let saved = std::fs::read_to_string(repo.baseline_file("before")).unwrap();
    assert!(saved.contains(r#""binary_bytes":100.0"#), "{saved}");

    std::fs::remove_file(repo.dir.join("bin")).unwrap();
    let err = fail(&tak_run(&repo.dir, &[NC, "--save-baseline", "before"], &[]));
    assert!(
        err.contains("not saving baseline `before`: a run missing a declared metric"),
        "{err}"
    );
    assert!(err.contains("1 metric(s) failed"), "{err}");
    assert_eq!(
        std::fs::read_to_string(repo.baseline_file("before")).unwrap(),
        saved,
        "the earlier baseline was replaced"
    );
    let err = fail(&tak_run(&repo.dir, &[NC, "--save-baseline", "after"], &[]));
    assert!(err.contains("not saving baseline `after`"), "{err}");
    assert!(!repo.baseline_file("after").exists());
    assert_no_notes(&repo.dir);
}

/// A baseline report shares `compare`'s renderer, custom-metrics table
/// included.
#[test]
fn a_baseline_report_shows_custom_metrics() {
    let repo = Repo::new();
    std::fs::write(
        repo.dir.join("tak.toml"),
        "[bench.startup]\ncmd = \"true\"\n[bench.startup.metric.binary_bytes]\nfile = \"bin\"\n",
    )
    .unwrap();
    std::fs::write(repo.dir.join("bin"), vec![0u8; 1000]).unwrap();
    ok(&tak_run(&repo.dir, &[NC, "--save-baseline", "before"], &[]));
    std::fs::write(repo.dir.join("bin"), vec![0u8; 1500]).unwrap();
    let out = tak_run(&repo.dir, &[NC, "--baseline", "before"], &[]);
    let report = stdout(ok(&out));
    assert!(
        report.contains("| startup | binary_bytes | 1,000 → 1,500 | +50.00% |"),
        "{report}"
    );
}
