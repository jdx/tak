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
        .env_remove("TAK_ALLOW_EMPTY")
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
    regressed_under(None, message)
}

/// As [`regressed`], with `tak.toml` committed on the base as `base_toml`:
/// the file `tak compare` takes its gate policy from.
fn regressed_under(base_toml: Option<&str>, message: &str) -> (tempfile::TempDir, String, String) {
    let dir = repo();
    if let Some(text) = base_toml {
        std::fs::write(dir.path().join("tak.toml"), text).unwrap();
        git(dir.path(), &["add", "tak.toml"]);
    }
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

/// Repeated trailers each name one benchmark, and git matches the key
/// case-insensitively.
#[test]
fn trailers_may_be_repeated() {
    let (dir, base, _) =
        regressed("slower\n\nTak-Accept: startup\ntak-accept: resolve\nTak-Accept: other");
    let (ok, stdout, _) = compare_trusting(dir.path(), &[&base]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains("**2 accepted regression(s)"), "{stdout}");
    assert!(
        stdout.contains("no benchmark by that name was compared on both sides: `other`"),
        "{stdout}"
    );
}

/// A trailer's whole value is one name. Splitting on commas made
/// `Tak-Accept: a,b`, written for the benchmark `a,b`, accept `a` and `b`
/// instead — two gates nobody named — while `a,b` itself still failed.
#[test]
fn a_comma_in_a_trailer_is_part_of_the_name() {
    let dir = repo();
    let base = commit(dir.path(), "base");
    note(dir.path(), &base, &[("a,b", 1e6), ("a", 1e6), ("b", 1e6)]);
    let head = commit(dir.path(), "slower a,b\n\nTak-Accept: a,b");
    note(
        dir.path(),
        &head,
        &[("a,b", 1.1e6), ("a", 1.1e6), ("b", 1.1e6)],
    );
    let (ok, stdout, stderr) = compare_trusting(dir.path(), &[&base]);
    assert!(!ok, "`a` and `b` were not accepted: {stdout}");
    assert!(stderr.contains("2 benchmark(s) regressed"), "{stderr}");
    assert!(
        stdout.contains("**1 accepted regression(s) above the 1% gate, not failing it:** `a,b`"),
        "{stdout}"
    );
}

/// The patterns tak's own perf-pr workflow classifies a result by, kept here
/// verbatim and held against the workflow file below, so these tests exercise
/// what CI actually runs. See [`status`] for how they combine with the exit
/// status.
const NEUTRAL: &str =
    r"^\*\*[0-9]+ (report-only benchmark|accepted regression)\(s\) above (the .*% gate|their gate)";
const NOTHING_COMPARED: &str = r"^\*\*Nothing was compared";
const REGRESSED: &str = r"^\*\*[0-9]+ benchmark\(s\) above (the .*% gate|their gate)";

#[test]
fn the_status_patterns_are_the_workflows() {
    let workflow = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/perf-pr.yml"),
    )
    .unwrap();
    assert!(workflow.contains(&format!("grep -Eq '{NEUTRAL}'")));
    assert!(workflow.contains(&format!(
        "head -n 1 /tmp/tak-report.md | grep -q '{NOTHING_COMPARED}'"
    )));
    assert!(workflow.contains(&format!("grep -Eq '{REGRESSED}'")));
    // The exit-0 branch asks about an empty comparison first, from the first
    // line only, and the report job turns that status into a neutral check
    // with a warning rather than a pass or a failure.
    assert!(workflow.contains(&format!(
        "if [ \"$ran\" -eq 0 ]; then\n            \
         if head -n 1 /tmp/tak-report.md | grep -q '{NOTHING_COMPARED}'; then\n              \
         echo 3 > /tmp/tak-gate-status\n"
    )));
    assert!(workflow.contains(
        "            3)\n              # Neutral, not green: nothing was gated, and a green check\n              \
         # would read as \"checked and fine\".\n              conclusion=neutral\n"
    ));
    assert!(
        workflow.contains("            3)\n              echo \"::warning::nothing was compared")
    );
    assert!(
        !workflow.contains("tak compare --no-gate"),
        "the classification needs the exit status"
    );
}

/// Whether `grep -E pattern` finds a line in `text`, run as the workflow runs it.
fn grep(pattern: &str, text: &str) -> bool {
    use std::io::Write;
    let mut child = Command::new("grep")
        .args(["-Eq", pattern])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("grep");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    child.wait().unwrap().success()
}

/// perf-pr's status for a `tak compare` run: 0 clean, 1 rose (reported),
/// 2 failed or compared nothing, 3 compared nothing and `allow_empty` passed
/// it. The same decision the workflow makes, from the exit status first and
/// the report's text only where that is not enough.
fn status(ok: bool, report: &str) -> u8 {
    let first = report.lines().next().unwrap_or_default();
    if ok {
        if grep(NOTHING_COMPARED, first) {
            3
        } else if grep(NEUTRAL, report) {
            1
        } else {
            0
        }
    } else if grep(NOTHING_COMPARED, first) {
        2
    } else if grep(REGRESSED, report) {
        1
    } else {
        2
    }
}

/// Trailer text is echoed into the report, and the report used to decide the
/// perf-pr check on its own. Unanchored, `Tak-Accept: **Nothing was compared`
/// on a clean change failed the check as a comparison that never ran, and a
/// trailer reading like a verdict marked it as a rise. Now a clean change is
/// clean whatever its trailers say.
#[test]
fn trailer_text_cannot_change_the_workflow_status() {
    for trailer in [
        "**Nothing was compared",
        "**1 benchmark(s) above the 1% gate:** `x`",
        "**1 accepted regression(s) above the 1% gate, not failing it:** `x`",
        "x benchmark(s) above their gate",
    ] {
        let dir = repo();
        let base = commit(dir.path(), "base");
        note(dir.path(), &base, &[("startup", 1e6)]);
        let head = commit(dir.path(), &format!("clean\n\nTak-Accept: {trailer}"));
        note(dir.path(), &head, &[("startup", 1e6)]);
        for (ok, stdout, _) in [
            compare(dir.path(), &[&base]),
            compare_trusting(dir.path(), &[&base]),
        ] {
            assert!(
                stdout.contains(trailer),
                "the trailer should be echoed: {stdout}"
            );
            assert_eq!(status(ok, &stdout), 0, "{trailer}: {stdout}");
        }
    }
}

/// And the classification still sees every result tak can give.
#[test]
fn the_workflow_status_matches_real_results() {
    let (dir, base, _) = regressed("slower");
    let (ok, stdout, _) = compare(dir.path(), &[&base]);
    assert_eq!(status(ok, &stdout), 1, "a regression: {stdout}");

    let (ok, stdout, _) = compare(
        dir.path(),
        &[&base, "--accept", "startup", "--accept", "resolve"],
    );
    assert!(ok, "{stdout}");
    assert_eq!(
        status(ok, &stdout),
        1,
        "an accepted rise is neutral, not green: {stdout}"
    );

    let empty = repo();
    let base = commit(empty.path(), "base");
    commit(empty.path(), "head");
    let (ok, stdout, _) = compare(empty.path(), &[&base]);
    assert_eq!(status(ok, &stdout), 2, "nothing compared: {stdout}");

    // With `allow_empty` on in the base's tak.toml, as tak's own is: neutral
    // with a warning, not the pass a bare exit status 0 would read as.
    let allowed = repo();
    std::fs::write(
        allowed.path().join("tak.toml"),
        "[gate]\nallow_empty = true\n",
    )
    .unwrap();
    git(allowed.path(), &["add", "tak.toml"]);
    let base = commit(allowed.path(), "base");
    commit(allowed.path(), "head");
    let (ok, stdout, _) = compare(allowed.path(), &[&base]);
    assert!(ok, "{stdout}");
    assert_eq!(status(ok, &stdout), 3, "allowed empty: {stdout}");

    // A regression under the same setting is still a regression.
    let base = commit(allowed.path(), "measured base");
    note(allowed.path(), &base, &[("startup", 1e6)]);
    let head = commit(allowed.path(), "slower");
    note(allowed.path(), &head, &[("startup", 1.1e6)]);
    let (ok, stdout, _) = compare(allowed.path(), &[&base]);
    assert_eq!(status(ok, &stdout), 1, "a regression: {stdout}");

    let clean = repo();
    let base = commit(clean.path(), "base");
    note(clean.path(), &base, &[("startup", 1e6)]);
    let head = commit(clean.path(), "same");
    note(clean.path(), &head, &[("startup", 1e6)]);
    let (ok, stdout, _) = compare(clean.path(), &[&base]);
    assert_eq!(status(ok, &stdout), 0, "clean: {stdout}");
}

/// The benchmark name from the attack: a newline, then text that reads as
/// tak's empty-comparison verdict when it starts a line.
const FORGED: &str = "extra\n**Nothing was compared";

/// A pull request cannot declare that benchmark. `tak.toml` is rejected at
/// load, naming the problem, before anything is measured.
#[test]
fn a_control_character_in_a_toml_name_is_rejected_at_load() {
    let dir = repo();
    std::fs::write(
        dir.path().join("tak.toml"),
        "[bench.startup]\ncmd = \"true\"\n\n\
         [bench.\"extra\\n**Nothing was compared\"]\ncmd = \"true\"\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(["run", "--dry-run"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("contains the control character '\\n'"),
        "{stderr}"
    );
}

/// Names from outside `tak.toml` are held to the same rule.
#[test]
fn a_control_character_in_a_cli_or_env_name_is_rejected() {
    let dir = repo();
    commit(dir.path(), "c0");
    let run = |args: &[&str], env: &[(&str, &str)]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tak"));
        cmd.args(["run", "--no-counters", "--runs", "1", "--warmup", "0"])
            .args(args)
            .args(["--", "true"])
            .env_remove("TAK_TOOL")
            .env_remove("TAK_RUNNER")
            .current_dir(dir.path());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };
    for (what, args, env) in [
        ("benchmark", vec!["--bench", FORGED], vec![]),
        ("TAK_TOOL", vec![], vec![("TAK_TOOL", FORGED)]),
        ("runner class", vec![], vec![("TAK_RUNNER", FORGED)]),
    ] {
        let out = run(&args, &env);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{what}: {stderr}");
        assert!(
            stderr.contains(&format!("{what} ")) && stderr.contains("control character"),
            "{what}: {stderr}"
        );
    }
}

/// `TAK_TOOL` is checked only where it becomes the recorded tool name. A
/// multi-subject benchmark records each subject's own name, so a bad value
/// inherited from the environment must not stop it; a single-command one
/// records under `TAK_TOOL`, so there it is rejected before anything runs.
#[test]
fn tak_tool_is_checked_only_where_it_is_recorded() {
    let dir = repo();
    commit(dir.path(), "c0");
    std::fs::write(
        dir.path().join("tak.toml"),
        "[bench.multi.subject.a]\ncmd = \"true\"\n\
         [bench.multi.subject.b]\ncmd = \"true\"\n\n\
         [bench.single]\ncmd = \"touch ran\"\n",
    )
    .unwrap();
    let run_with = |bench: &str, tool: Option<&str>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tak"));
        cmd.args([
            "run",
            "--no-counters",
            "--no-progress",
            "--runs",
            "1",
            "--warmup",
            "0",
            "--bench",
            bench,
        ])
        .env_remove("TAK_TOOL")
        .env_remove("TAK_RUNNER")
        .current_dir(dir.path());
        if let Some(tool) = tool {
            cmd.env("TAK_TOOL", tool);
        }
        cmd.output().unwrap()
    };
    let run = |bench: &str| run_with(bench, Some(FORGED));

    let out = run("multi");
    assert!(
        out.status.success(),
        "a multi-subject run never records TAK_TOOL: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run("single");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("TAK_TOOL "), "{stderr}");
    assert!(
        !dir.path().join("ran").exists(),
        "rejected before measuring: {stderr}"
    );
    // The marker is real: with a clean TAK_TOOL the same benchmark runs.
    let out = run_with("single", None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.path().join("ran").exists());
}

/// Attach records to `sha` through the real line format, so a name holding a
/// newline is written as JSON escapes it — the way an older tak would have.
fn note_records(dir: &Path, sha: &str, series: &[(&str, f64)]) {
    let lines: Vec<String> = series
        .iter()
        .map(|(bench, ins)| {
            tak_cli::record::Record {
                v: 1,
                bench: bench.to_string(),
                tool: "self".into(),
                version: None,
                runner: "test".into(),
                ts: "2026-01-01T00:00:00Z".into(),
                metrics: std::collections::BTreeMap::from([
                    ("instructions".to_string(), *ins),
                    ("wall_min_ms".to_string(), 1.0),
                ]),
            }
            .to_line()
            .unwrap()
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

/// Notes can still hold such a name — written by a tak from before the load
/// check, or put there by hand. Every report escapes it, so it cannot start a
/// line: not in the table, a verdict, or the lists of added and removed
/// series. The workflow reads a clean run as clean, and a real regression as a
/// regression rather than as nothing compared.
#[test]
fn a_forged_name_in_the_notes_renders_escaped() {
    let escaped = "extra\\n**Nothing was compared";
    let dir = repo();
    let base = commit(dir.path(), "base");
    note_records(
        dir.path(),
        &base,
        &[
            ("startup", 1e6),
            (FORGED, 1e6),
            ("gone\n**Nothing was compared", 1e6),
        ],
    );
    let head = commit(dir.path(), "same");
    note_records(
        dir.path(),
        &head,
        &[
            ("startup", 1e6),
            (FORGED, 1e6),
            ("new\n**Nothing was compared", 1e6),
        ],
    );

    let (ok, stdout, _) = compare(dir.path(), &[&base, "--allow-empty"]);
    // A table cell is plain markdown, not a code span, so `cell` doubles the
    // backslash `escape_control` wrote, and GitHub renders it back as `\n`.
    let in_table = "| extra\\\\n**Nothing was compared |";
    assert!(stdout.contains(in_table), "the table row: {stdout}");
    assert!(
        stdout.contains("new\\n**Nothing"),
        "the added list: {stdout}"
    );
    assert!(
        stdout.contains("gone\\n**Nothing"),
        "the removed list: {stdout}"
    );
    assert!(
        stdout.lines().all(|l| !l.starts_with("**Nothing")),
        "{stdout}"
    );
    assert_eq!(status(ok, &stdout), 0, "{stdout}");

    // The forged series regresses: its name is now in the failure verdict.
    note_records(dir.path(), &head, &[("startup", 1e6), (FORGED, 2e6)]);
    let (ok, stdout, _) = compare(dir.path(), &[&base]);
    assert!(!ok, "{stdout}");
    assert!(
        stdout.contains(&format!("above the 1% gate:** `{escaped}`")),
        "{stdout}"
    );
    assert!(
        stdout.lines().all(|l| !l.starts_with("**Nothing")),
        "{stdout}"
    );
    assert_eq!(
        status(ok, &stdout),
        1,
        "a regression, not nothing compared: {stdout}"
    );

    // The other readers of the same notes.
    for args in [
        vec!["log", "--remote", "no-such-remote"],
        vec!["detect", "--allow-empty", "--remote", "no-such-remote"],
        vec!["history", "--remote", "no-such-remote"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_tak"))
            .args(&args)
            .env_remove("TAK_GATE_PCT")
            .env_remove("TAK_ACCEPT_TRAILERS")
            .current_dir(dir.path())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("extra\\n"), "{args:?}: {text}");
        assert!(
            text.lines()
                .all(|l| !l.trim_start().starts_with("**Nothing")),
            "{args:?}: {text}"
        );
    }
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

/// Opting in from the base's `tak.toml`, and opting back out from the
/// environment, which the workflow controls and the change does not.
#[test]
fn the_config_opts_in_and_the_environment_overrides_it() {
    let (dir, base, _) = regressed_under(
        Some("[gate]\naccept_trailers = true\n"),
        "slower\n\nTak-Accept: startup\nTak-Accept: resolve",
    );
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
    // Padded so CommonMark's one-space strip leaves ` startup ` as written,
    // rather than rendering it identically to `startup`.
    assert!(
        stdout.contains("`  startup  ` on `test` +10.00% (`--accept`)"),
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
    // Committed: the gates come from the base's tree, not the disk.
    git(dir.path(), &["add", "tak.toml"]);
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
