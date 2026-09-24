//! Multi-subject benchmarks, observed from the outside.
//!
//! The schedule is unit-tested in `measure.rs`. These run the real `tak`
//! binary against a `tak.toml` whose subjects log what they do, and check what
//! actually happened: the order samples were taken in, that prepare ran before
//! each of them, and what reached the subject's environment.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

/// A scratch project with a `tak.toml` and a log the subjects append to.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, toml: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tak-subjects-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tak.toml"), toml).unwrap();
        Project { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tak"))
            .arg("run")
            .args(args)
            .current_dir(&self.dir)
            .env("GITHUB_TOKEN", "sentinel-must-not-leak")
            .envs(env.iter().copied())
            .output()
            .expect("failed to run tak")
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.join(p)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// A subject table whose command logs `run:<name>` and whose prepare logs
/// `prep:<name>`, both into `log` in the project directory.
fn logging_subject(name: &str, runs: u32) -> String {
    format!(
        r#"
[bench.cmp.subject.{name}]
cmd = ["sh", "-c", "echo run:{name} >> log"]
prepare = ["sh", "-c", "echo prep:{name} >> log"]
runs = {runs}
"#
    )
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn samples_are_interleaved_and_each_is_prepared() {
    let p = Project::new(
        "interleave",
        &format!(
            "[bench.cmp]\nwarmup = 1\n{}{}",
            logging_subject("a", 6),
            logging_subject("b", 6)
        ),
    );
    let out = p.run(&["--seed", "11"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let log = p.log();
    // Every sample, warmup included, is its prepare immediately followed by
    // its run.
    assert_eq!(log.len(), 2 * (1 + 6) * 2);
    for pair in log.chunks(2) {
        let name = pair[0].strip_prefix("prep:").expect("prepare comes first");
        assert_eq!(pair[1], format!("run:{name}"));
    }

    // Not one subject's runs then the other's: `a` and `b` alternate within
    // rounds, so neither has all its samples before the other starts.
    let runs: Vec<&str> = log.iter().filter_map(|l| l.strip_prefix("run:")).collect();
    let last_a = runs.iter().rposition(|n| *n == "a").unwrap();
    let first_b = runs.iter().position(|n| *n == "b").unwrap();
    let last_b = runs.iter().rposition(|n| *n == "b").unwrap();
    let first_a = runs.iter().position(|n| *n == "a").unwrap();
    assert!(
        first_b < last_a && first_a < last_b,
        "not interleaved: {runs:?}"
    );
}

#[test]
fn the_same_seed_repeats_the_order() {
    let toml = format!(
        "[bench.cmp]\nwarmup = 0\n{}{}{}",
        logging_subject("a", 8),
        logging_subject("b", 8),
        logging_subject("c", 8)
    );
    let first = Project::new("seed1", &toml);
    let second = Project::new("seed2", &toml);
    assert!(first.run(&["--seed", "99"]).status.success());
    assert!(second.run(&["--seed", "99"]).status.success());
    assert_eq!(first.log(), second.log());
}

#[test]
fn the_export_has_every_sample_of_every_subject() {
    let p = Project::new(
        "export",
        &format!(
            "[bench.cmp]\nwarmup = 0\n{}{}",
            logging_subject("a", 4),
            logging_subject("b", 2)
        ),
    );
    let out = p.run(&["--export-json", "results.json"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("results.json")).unwrap()).unwrap();
    let results = json["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for (r, (name, runs)) in results.iter().zip([("a", 4), ("b", 2)]) {
        assert_eq!(r["bench"], "cmp");
        assert_eq!(r["subject"], name);
        assert_eq!(r["command"], name);
        assert_eq!(r["times"].as_array().unwrap().len(), runs);
        for key in ["mean", "stddev", "median", "min", "max"] {
            assert!(r[key].as_f64().unwrap() >= 0.0, "{key}");
        }
    }
}

/// One broken competitor must not throw away the others' samples, and must
/// not pass silently either.
#[test]
fn a_failing_subject_is_dropped_and_the_run_fails() {
    let p = Project::new(
        "failing",
        &format!(
            "[bench.cmp]\nwarmup = 0\n{}\n[bench.cmp.subject.broken]\ncmd = [\"false\"]\n",
            logging_subject("a", 3)
        ),
    );
    let out = p.run(&["--export-json", "results.json"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("broken"), "{}", stderr(&out));

    let runs = p.log().iter().filter(|l| l.starts_with("run:a")).count();
    assert_eq!(runs, 3, "the healthy subject still got every sample");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("results.json")).unwrap()).unwrap();
    let subjects: Vec<&str> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, ["a"]);
}

/// Declared variables and directories reach the subject; the forge token the
/// scrub removes still does not.
#[test]
fn a_subject_sees_its_env_and_dir_but_not_the_token() {
    let p = Project::new(
        "env",
        r#"
[bench.cmp]
warmup = 0
runs = 1
env = { SHARED = "bench" }

[bench.cmp.subject.x]
cmd = ["sh", "-c", "printf '%s %s %s %s' \"$SHARED\" \"$OWN\" \"${GITHUB_TOKEN-ABSENT}\" \"$(basename \"$PWD\")\" > ../seen"]
dir = "sub"
env = { OWN = "subject" }
"#,
    );
    std::fs::create_dir(p.path("sub")).unwrap();
    let out = p.run(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(p.path("seen")).unwrap(),
        "bench subject ABSENT sub"
    );
}

#[test]
fn subject_selects_and_rejects_unknown_names() {
    let p = Project::new(
        "select",
        &format!(
            "[bench.cmp]\nwarmup = 0\n{}{}",
            logging_subject("a", 2),
            logging_subject("b", 2)
        ),
    );
    assert!(p.run(&["--subject", "b"]).status.success());
    assert!(p.log().iter().all(|l| l.ends_with(":b")), "{:?}", p.log());

    let out = p.run(&["--subject", "nope"]);
    assert!(!out.status.success());
    let msg = stderr(&out);
    assert!(msg.contains("nope") && msg.contains("a, b"), "{msg}");
}

/// Single-command benchmarks are what every existing tak.toml declares; they
/// keep their output and their `self` series.
#[test]
fn a_single_command_benchmark_is_unchanged() {
    let p = Project::new(
        "single",
        "[bench.one]\ncmd = [\"true\"]\nruns = 2\nwarmup = 0\n",
    );
    let out = p.run(&["--no-counters", "--export-json", "r.json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("  one  true"), "{stdout}");
    assert!(!stdout.contains("interleaved"), "{stdout}");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    assert_eq!(json["results"][0]["command"], "one");
    assert_eq!(json["results"][0]["subject"], "self");
}

/// `dir` moves where the subject runs, not where `./bin/...` is found — the
/// project's own binary stays reachable from a fixture directory.
#[test]
fn a_relative_program_is_found_from_the_config_not_from_dir() {
    use std::os::unix::fs::PermissionsExt;

    let p = Project::new(
        "anchor",
        r#"
[bench.cmp]
warmup = 0
runs = 1

[bench.cmp.subject.x]
cmd = ["./bin/probe"]
prepare = ["./bin/probe"]
dir = "fixture"
"#,
    );
    std::fs::create_dir(p.path("bin")).unwrap();
    std::fs::create_dir(p.path("fixture")).unwrap();
    let probe = p.path("bin/probe");
    std::fs::write(&probe, "#!/bin/sh\nbasename \"$PWD\" >> ../ran\n").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = p.run(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(p.path("ran")).unwrap(),
        "fixture\nfixture\n",
        "prepare and cmd both ran, in `dir`"
    );
}

/// Outside a terminal, progress is plain lines on stderr, ending at 100%,
/// and `--no-progress` silences it.
#[test]
fn progress_is_logged_and_can_be_turned_off() {
    let p = Project::new(
        "progress",
        &format!(
            "[bench.cmp]\nwarmup = 0\n{}{}",
            logging_subject("a", 3),
            logging_subject("b", 3)
        ),
    );
    let out = p.run(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("  cmp: 6/6 100%"), "{err}");
    assert!(err.contains("elapsed"), "{err}");
    assert!(!err.contains('\r'), "no terminal redraws in a log: {err:?}");

    let quiet = p.run(&["--no-progress"]);
    assert!(quiet.status.success());
    assert!(!stderr(&quiet).contains("elapsed"), "{}", stderr(&quiet));
}

/// A multi-subject summary is one line per subject, not a metric per line.
#[test]
fn a_multi_subject_summary_is_one_line_per_subject() {
    let p = Project::new(
        "summary",
        &format!(
            "[bench.cmp]\nwarmup = 0\n{}{}",
            logging_subject("a", 2),
            logging_subject("bb", 2)
        ),
    );
    let out = p.run(&["--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<&str> = stdout.lines().filter(|l| l.contains(" min ")).collect();
    assert_eq!(rows.len(), 2, "{stdout}");
    assert!(
        rows[0].trim_start().starts_with("a ") && rows[0].contains("n=2"),
        "{stdout}"
    );
    assert!(!stdout.contains("wall_min_ms"), "{stdout}");
}

/// `--runs auto` works on the command line against a benchmark that never
/// declared it, using the default limits.
#[test]
fn runs_auto_can_be_given_on_the_command_line() {
    let p = Project::new(
        "auto-cli",
        &format!(
            "[bench.cmp]\nwarmup = 0\nmax_runs = 7\n{}",
            logging_subject("a", 3)
        ),
    );
    let out = p.run(&["--runs", "auto", "--no-progress", "--export-json", "r.json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    // A near-instant sample fits the budget many times over: the ceiling.
    assert_eq!(json["results"][0]["times"].as_array().unwrap().len(), 7);

    let bad = p.run(&["--runs", "lots"]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("auto"), "{}", stderr(&bad));
}

/// Shared subjects, defaults and templates end to end: the values reach the
/// command without a shell, and each benchmark gets its own render.
#[test]
fn templates_fill_commands_from_the_environment() {
    let p = Project::new(
        "templates",
        r#"
[defaults]
warmup = 0
runs = 1
dir = "{{ env.WORK }}"

[subject.a]
cmd = ["sh", "-c", "echo $0 $1 >> log", "{{ bench }}-{{ subject }}", "{{ vars.tag }}"]
vars = { tag = "{{ env.TAG }}" }

[bench.one]
subjects = ["a"]

[bench.two]
subjects = ["a"]
"#,
    );
    let work = p.path("work");
    std::fs::create_dir(&work).unwrap();
    let out = p.run_env(
        &["--no-progress"],
        &[("WORK", work.to_str().unwrap()), ("TAG", "t1")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let log = std::fs::read_to_string(work.join("log")).unwrap();
    assert_eq!(log, "one-a t1\ntwo-a t1\n");
}

/// A variable nobody set fails the run before any sample is taken, naming
/// the benchmark and subject — but only for benchmarks being run.
#[test]
fn an_unset_variable_fails_before_measuring_only_where_it_is_used() {
    let p = Project::new(
        "unset",
        r#"
[bench.needs]
warmup = 0
runs = 1
cmd = ["sh", "-c", "echo ran >> log", "{{ env.TAK_TEST_NEVER_SET }}"]

[bench.fine]
warmup = 0
runs = 1
cmd = ["true"]
"#,
    );
    let out = p.run(&["--no-progress"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("needs"), "{}", stderr(&out));
    assert!(p.log().is_empty(), "nothing ran");

    assert!(
        p.run(&["--no-progress", "--bench", "fine"])
            .status
            .success()
    );
}

/// Subjects left out with --subject are not rendered, so a variable only
/// they need does not have to be set.
#[test]
fn an_excluded_subject_is_not_rendered() {
    let p = Project::new(
        "exclude-render",
        r#"
[bench.cmp]
warmup = 0
runs = 1

[bench.cmp.subject.ok]
cmd = ["true"]

[bench.cmp.subject.needs]
cmd = ["{{ env.TAK_TEST_NEVER_SET }}"]
"#,
    );
    let out = p.run(&["--no-progress", "--subject", "ok"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

/// --config reads the named file instead of searching, and commands resolve
/// relative to that file, not to where tak was started.
#[test]
fn config_names_the_file_to_read() {
    let p = Project::new("config-flag", "[bench.root]\ncmd = [\"false\"]\n");
    std::fs::create_dir(p.path("sub")).unwrap();
    std::fs::write(
        p.path("sub/other.toml"),
        "[bench.other]\nwarmup = 0\nruns = 1\ncmd = [\"sh\", \"-c\", \"pwd > where\"]\n",
    )
    .unwrap();
    let out = p.run(&["--no-progress", "--config", "sub/other.toml"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let where_ = std::fs::read_to_string(p.path("sub/where")).unwrap();
    assert!(where_.trim_end().ends_with("sub"), "{where_}");

    // A relative program path in a config named relatively, from its own
    // directory: the bare-filename case, whose parent is empty.
    std::fs::create_dir(p.path("sub/bin")).unwrap();
    std::fs::write(p.path("sub/bin/probe"), "#!/bin/sh\npwd > where2\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            p.path("sub/bin/probe"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::write(
        p.path("sub/rel.toml"),
        "[bench.rel]\nwarmup = 0\nruns = 1\ncmd = [\"./bin/probe\"]\n",
    )
    .unwrap();
    let rel = Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(["run", "--no-progress", "--config", "rel.toml"])
        .current_dir(p.path("sub"))
        .output()
        .unwrap();
    assert!(rel.status.success(), "{}", stderr(&rel));
    assert!(p.path("sub/where2").exists());

    let missing = p.run(&["--config", "nope.toml"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("nope.toml"),
        "{}",
        stderr(&missing)
    );
}

/// --dry-run prints what would run, templates rendered and overrides
/// applied, and runs nothing.
#[test]
fn dry_run_shows_the_resolved_plan_without_running() {
    let p = Project::new(
        "dry-run",
        r#"
[defaults]
runs = "auto"
env = { HOME = "{{ env.TAK_TEST_BASE }}/home-{{ subject }}" }

[subject.a]
cmd = ["sh", "-c", "echo ran >> log"]

[bench.cmp]
subjects = ["a"]
"#,
    );
    let out = p.run_env(
        &["--dry-run", "--warmup", "4"],
        &[("TAK_TEST_BASE", "/base")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cmp"), "{stdout}");
    assert!(
        stdout.contains("cmd      sh -c 'echo ran >> log'"),
        "{stdout}"
    );
    assert!(stdout.contains("HOME=/base/home-a"), "{stdout}");
    assert!(
        stdout.contains("runs     auto (30s budget, 5..=50), warmup 4"),
        "{stdout}"
    );
    assert!(p.log().is_empty(), "nothing ran");
}

/// The export records how the run was made.
#[test]
fn the_export_records_version_seed_and_runner() {
    let p = Project::new(
        "export-meta",
        &format!("[bench.cmp]\nwarmup = 0\n{}", logging_subject("a", 1)),
    );
    let out = p.run(&[
        "--no-progress",
        "--seed",
        "77",
        "--runner",
        "test-class",
        "--export-json",
        "r.json",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    assert_eq!(
        json["seed"], "77",
        "a string, so large seeds survive JSON readers"
    );
    assert_eq!(json["runner"], "test-class");
    assert_eq!(json["tak_version"], env!("CARGO_PKG_VERSION"));
    assert!(json["time"].as_str().unwrap().ends_with('Z'));
    assert_eq!(json["results"].as_array().unwrap().len(), 1);
}

/// `when` leaves a subject or benchmark out before it is rendered, says so,
/// and refuses a --subject that its own `when` switches off.
#[test]
fn when_skips_subjects_and_benchmarks() {
    let p = Project::new(
        "when",
        r#"
[defaults]
warmup = 0
runs = 1

[subject.here]
cmd = ["sh", "-c", "echo here >> log"]

[subject.absent]
when = '(env.TAK_TEST_ABSENT_BIN ?? "") != ""'
cmd = ["{{ env.TAK_TEST_ABSENT_BIN }}"]

[bench.cmp]
subjects = ["here", "absent"]

[bench.never]
when = "false"
cmd = ["sh", "-c", "echo never >> log"]
"#,
    );
    let out = p.run(&["--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.log(), ["here"]);
    let err = stderr(&out);
    assert!(err.contains("skipping cmp (absent)"), "{err}");
    assert!(err.contains("skipping never"), "{err}");

    // Set, the same subject runs: the condition really reads the variable.
    let present = p.run_env(
        &["--no-progress", "--subject", "absent"],
        &[("TAK_TEST_ABSENT_BIN", "true")],
    );
    assert!(present.status.success(), "{}", stderr(&present));

    let asked = p.run(&["--no-progress", "--subject", "absent"]);
    assert!(!asked.status.success());
    assert!(
        stderr(&asked).contains("`when` is false"),
        "{}",
        stderr(&asked)
    );
}

/// A spike in a subject's samples is reported on stderr.
#[test]
fn an_outlier_is_warned_about() {
    let p = Project::new(
        "outlier",
        r#"
[bench.spiky]
warmup = 0
runs = 7
# The third sample sleeps; the rest do not.
cmd = ["sh", "-c", "n=$(cat n 2>/dev/null || echo 0); echo $((n+1)) > n; [ \"$n\" = 2 ] && sleep 0.3; true"]
"#,
    );
    let out = p.run(&["--no-progress", "--no-counters"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("outliers"), "{}", stderr(&out));
}

/// --dry-run reflects --no-counters the way a real run would.
#[test]
fn dry_run_honours_no_counters() {
    let p = Project::new("dry-counters", "[bench.one]\ncmd = [\"true\"]\n");
    let on = p.run(&["--dry-run"]);
    assert!(String::from_utf8_lossy(&on.stdout).contains("counters on"));
    let off = p.run(&["--dry-run", "--no-counters"]);
    assert!(!String::from_utf8_lossy(&off.stdout).contains("counters on"));
}

/// Only the subjects being run have their conditions evaluated, a subject
/// in a switched-off benchmark is reported as such, and a run where every
/// selected benchmark is switched off says so.
#[test]
fn when_is_decided_only_for_what_runs() {
    let p = Project::new(
        "when-scope",
        r#"
[defaults]
warmup = 0
runs = 1

[bench.cmp.subject.here]
cmd = ["true"]

[bench.cmp.subject.odd]
when = '"not a boolean"'
cmd = ["true"]

[bench.off]
when = "false"
[bench.off.subject.hidden]
cmd = ["true"]

[bench.broken]
when = '"not a boolean either"'
[bench.broken.subject.elsewhere]
cmd = ["true"]
"#,
    );
    let out = p.run(&["--no-progress", "--subject", "here"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("skipping off"),
        "unrelated benchmark: {}",
        stderr(&out)
    );

    let hidden = p.run(&["--no-progress", "--subject", "hidden"]);
    assert!(!hidden.status.success());
    assert!(
        stderr(&hidden).contains("`when` is false for it in benchmark `off`"),
        "{}",
        stderr(&hidden)
    );

    let none = p.run(&["--no-progress", "--bench", "off"]);
    assert!(none.status.success(), "{}", stderr(&none));
    // Recording or exporting nothing is a failure, so CI notices.
    let export = p.run(&["--no-progress", "--bench", "off", "--export-json", "r.json"]);
    assert!(!export.status.success());
    assert!(
        stderr(&export).contains("nothing to export"),
        "{}",
        stderr(&export)
    );
    // ...but a dry run writes nothing either way, so it does not fail.
    let dry = p.run(&[
        "--dry-run",
        "--bench",
        "off",
        "--record",
        "--export-json",
        "r.json",
    ]);
    assert!(dry.status.success(), "{}", stderr(&dry));
    assert!(String::from_utf8_lossy(&none.stdout).contains("nothing to run"));
    assert!(stderr(&none).contains("skipping off"), "{}", stderr(&none));
}

/// A subject switched off in one benchmark is still measured by another
/// that runs it; --subject does not abort over the first.
#[test]
fn a_subject_off_in_one_benchmark_still_runs_in_another() {
    let p = Project::new(
        "when-across",
        r#"
[defaults]
warmup = 0
runs = 1

[bench.a.subject.x]
when = "false"
cmd = ["sh", "-c", "echo a >> log"]

[bench.b.subject.x]
cmd = ["sh", "-c", "echo b >> log"]
"#,
    );
    let out = p.run(&["--no-progress", "--subject", "x"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.log(), ["b"]);
}

/// `setup` runs once per subject, before any subject's first sample — not
/// once per sample like prepare — from the directory holding tak.toml, so it
/// can create the subject's `dir`, with the subject's environment.
#[test]
fn setup_runs_once_per_subject_before_any_sample() {
    let p = Project::new(
        "setup",
        r#"
[defaults]
warmup = 2
runs = 3
env = { WHO = "default" }
# Creates the directory every other command runs in, and logs where it ran.
setup = ["sh", "-c", "mkdir -p work/{{ subject }} && echo \"setup:{{ subject }}:$WHO:$(basename \"$PWD\")\" >> log"]
dir = "work/{{ subject }}"

[bench.cmp.subject.a]
cmd = ["sh", "-c", "echo run:a >> ../../log"]
prepare = ["sh", "-c", "echo prep:a >> ../../log"]
env = { WHO = "a" }

[bench.cmp.subject.b]
cmd = ["sh", "-c", "echo run:b >> ../../log"]
prepare = ["sh", "-c", "echo prep:b >> ../../log"]
"#,
    );
    let root = p.dir.file_name().unwrap().to_string_lossy().into_owned();
    let out = p.run(&["--seed", "5"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let log = p.log();
    assert_eq!(
        &log[..2],
        [
            format!("setup:a:a:{root}"),
            format!("setup:b:default:{root}")
        ],
        "both setups, first, in tak.toml's directory: {log:?}"
    );
    let rest = &log[2..];
    assert!(rest.iter().all(|l| !l.starts_with("setup:")), "{log:?}");
    // Two subjects, each 2 warmups + 3 runs, each a prepare and a run.
    assert_eq!(rest.len(), 2 * (2 + 3) * 2, "{log:?}");
    assert!(rest[0].starts_with("prep:"), "{log:?}");
    let err = stderr(&out);
    assert!(err.contains("setup a"), "setup shown in progress: {err}");
}

/// A benchmark-level setup is replaced by a subject's own, like prepare.
#[test]
fn a_subject_setup_replaces_the_benchmark_setup() {
    let p = Project::new(
        "setup-override",
        r#"
[bench.cmp]
warmup = 0
runs = 1
setup = ["sh", "-c", "echo setup:bench:{{ subject }} >> log"]

[bench.cmp.subject.a]
cmd = ["true"]

[bench.cmp.subject.b]
cmd = ["true"]
setup = ["sh", "-c", "echo setup:own:b >> log"]
"#,
    );
    let out = p.run(&["--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.log(), ["setup:bench:a", "setup:own:b"]);
}

/// A failing setup drops its subject like a failing prepare: the others are
/// still measured, the run fails, and --record writes nothing.
#[test]
fn a_failing_setup_drops_the_subject() {
    let p = Project::new(
        "setup-fails",
        &format!(
            "[bench.cmp]\nwarmup = 1\n{}\n[bench.cmp.subject.broken]\ncmd = [\"sh\", \"-c\", \"echo run:broken >> log\"]\nsetup = [\"sh\", \"-c\", \"echo no fixture >&2; exit 4\"]\n",
            logging_subject("a", 3)
        ),
    );
    let out = p.run(&["--no-progress", "--record"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("cmp (broken) dropped") && err.contains("setup") && err.contains("no fixture"),
        "{err}"
    );
    assert!(err.contains("not recording"), "{err}");
    let log = p.log();
    assert!(!log.iter().any(|l| l == "run:broken"), "{log:?}");
    assert_eq!(log.iter().filter(|l| *l == "run:a").count(), 1 + 3);
}

/// Setup is only run for subjects that will be measured: not for one left
/// out by --subject, one whose `when` is false, or a benchmark switched off.
/// A subject in two benchmarks is set up once for each.
#[test]
fn setup_runs_only_for_measured_subjects() {
    let p = Project::new(
        "setup-skipped",
        r#"
[defaults]
warmup = 0
runs = 1
setup = ["sh", "-c", "echo {{ bench }}:{{ subject }} >> log"]

[subject.a]
cmd = ["true"]

[subject.b]
cmd = ["true"]

[subject.off]
when = "false"
cmd = ["true"]

[bench.one]
subjects = ["a", "b", "off"]

[bench.two]
subjects = ["a"]

[bench.never]
when = "false"
subjects = ["a"]
"#,
    );
    let out = p.run(&["--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.log(), ["one:a", "one:b", "two:a"]);

    std::fs::remove_file(p.path("log")).unwrap();
    let out = p.run(&["--no-progress", "--subject", "b"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.log(), ["one:b"]);
}

/// --dry-run shows the rendered setup and where it would run, and runs it
/// no more than anything else.
#[test]
fn dry_run_shows_setup_without_running_it() {
    let p = Project::new(
        "setup-dry",
        r#"
[bench.cmp.subject.a]
cmd = ["true"]
setup = ["sh", "-c", "echo setup:{{ subject }} >> log"]
"#,
    );
    let out = p.run(&["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // tak finds tak.toml from its working directory, which the OS reports
    // with symlinks resolved: on macOS the temp dir is under /var, a link to
    // /private/var.
    let root = p.dir.canonicalize().unwrap();
    assert!(
        stdout.contains(&format!(
            "setup    sh -c 'echo setup:a >> log'  (in {})",
            root.display()
        )),
        "{stdout}"
    );
    assert!(p.log().is_empty(), "nothing ran");
}

/// A check that fails on some samples is counted, reported and exported, and
/// the subject keeps every sample: how often a subject gets the work wrong is
/// the result, not a reason to drop it.
#[test]
fn a_failing_check_is_counted_not_fatal() {
    let p = Project::new(
        "check",
        r#"
[bench.cmp]
warmup = 1
runs = 6
# Passes when the subject's counter file has an even number of lines. The
# warmup is run 1 and is never checked.
check = ["sh", "-c", "echo {{ subject }} >> checked; test $(( $(wc -l < n-{{ subject }}) % 2 )) = 1 || { echo even >&2; exit 1; }"]

# Two lines per run: every count is even, so every check fails.
[bench.cmp.subject.broken]
cmd = ["sh", "-c", "echo x >> n-broken; echo x >> n-broken"]

# One line per run: odd on runs 3, 5 and 7.
[bench.cmp.subject.flaky]
cmd = ["sh", "-c", "echo x >> n-flaky"]

[bench.cmp.subject.fine]
cmd = ["sh", "-c", "echo x >> n-fine"]
check = ["true"]
"#,
    );
    let out = p.run(&["--no-progress", "--export-json", "r.json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = |name: &str| {
        stdout
            .lines()
            .find(|l| l.trim_start().starts_with(name))
            .unwrap_or_else(|| panic!("no line for {name}: {stdout}"))
            .to_string()
    };
    assert!(line("flaky").contains("n=6  checks 3/6"), "{stdout}");
    assert!(line("broken").contains("checks 0/6"), "{stdout}");
    assert!(line("fine").contains("checks 6/6"), "{stdout}");
    let err = stderr(&out);
    assert!(
        err.contains("cmp (flaky): check failed after 3 of 6 samples (sample 1, 3, 5)"),
        "{err}"
    );
    assert!(err.contains("check `sh` exited with"), "{err}");
    assert!(!err.contains("cmp (fine): check"), "{err}");
    assert!(!err.contains("dropped"), "{err}");

    // Checked after each timed sample, never after the warmup.
    let checked = std::fs::read_to_string(p.path("checked")).unwrap();
    assert_eq!(checked.lines().filter(|l| *l == "flaky").count(), 6);

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    let by = |name: &str| {
        json["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["subject"] == name)
            .unwrap()
            .clone()
    };
    let flaky = by("flaky");
    assert_eq!(flaky["times"].as_array().unwrap().len(), 6);
    assert_eq!(
        flaky["checks"],
        serde_json::json!({
            "passed": 3,
            "total": 6,
            "samples": [false, true, false, true, false, true]
        })
    );
    assert_eq!(by("broken")["checks"]["passed"], 0);
    assert_eq!(by("fine")["checks"]["passed"], 6);
}

/// A single-command benchmark reports its checks with its other numbers, and
/// a benchmark without a check exports no `checks` at all.
#[test]
fn a_single_command_reports_its_checks() {
    let p = Project::new(
        "check-single",
        r#"
[bench.one]
cmd = ["sh", "-c", "echo x >> n"]
check = "true"
warmup = 0
runs = 2

[bench.two]
cmd = ["true"]
warmup = 0
runs = 1
"#,
    );
    let out = p.run(&["--no-progress", "--export-json", "r.json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .lines()
            .any(|l| l.split_whitespace().collect::<Vec<_>>() == ["checks", "2/2"]),
        "{stdout}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    let results = json["results"].as_array().unwrap();
    assert_eq!(results[0]["checks"]["total"], 2);
    assert!(results[1].get("checks").is_none(), "{}", results[1]);
}

/// --dry-run shows the check each subject would run, rendered and anchored.
#[test]
fn dry_run_shows_the_check() {
    let p = Project::new(
        "check-dry",
        r#"
[defaults]
check = ["./bin/verify", "{{ subject }}"]

[bench.cmp.subject.a]
cmd = ["sh", "-c", "echo ran >> log"]
"#,
    );
    let out = p.run(&["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // tak anchors paths at the directory it found tak.toml in, which it
    // reaches through the resolved working directory: on macOS the temp dir
    // under /var is really /private/var, so compare against the canonical path.
    let verify = p.dir.canonicalize().unwrap().join("./bin/verify");
    assert!(
        stdout.contains(&format!("check    {} a", verify.display())),
        "{stdout}"
    );
    assert!(p.log().is_empty(), "nothing ran");
}

/// With `runs = "auto"` and no warmups, the first timed sample is the pilot
/// that sizes the run, taken before the others. Its check verdict is still
/// the first one exported, next to the first time.
#[test]
fn the_auto_pilot_sample_is_checked_in_order() {
    let p = Project::new(
        "check-pilot",
        r#"
[bench.cmp]
runs = "auto"
warmup = 0
budget = "1s"
min_runs = 3
max_runs = 4
# Fails only on its first invocation, which is the pilot's.
check = ["sh", "-c", "echo x >> checks; test $(( $(wc -l < checks) )) != 1"]

[bench.cmp.subject.a]
cmd = ["true"]
"#,
    );
    let out = p.run(&["--no-progress", "--export-json", "r.json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    let r = &json["results"][0];
    let times = r["times"].as_array().unwrap().len();
    assert!((3..=4).contains(&times), "{r}");
    assert_eq!(r["checks"]["total"], times, "{r}");
    let samples: Vec<bool> = r["checks"]["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_bool().unwrap())
        .collect();
    assert_eq!(samples.len(), times, "{r}");
    assert!(!samples[0], "the pilot's failure comes first: {r}");
    assert!(samples[1..].iter().all(|&ok| ok), "{r}");
    assert_eq!(r["checks"]["passed"], times - 1, "{r}");
}

/// A failed check stops --record: git notes keep timings without verdicts, so
/// recording would store a failed sample's time as if it had passed. The
/// export is still written, and without --record the run succeeds.
#[test]
fn a_failing_check_stops_record_but_not_the_run() {
    let p = Project::new(
        "check-record",
        r#"
[bench.cmp]
warmup = 0
runs = 2

[bench.cmp.subject.good]
cmd = ["true"]
check = ["true"]

[bench.cmp.subject.bad]
cmd = ["true"]
check = ["false"]
"#,
    );
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&p.dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let ident = [
        ("GIT_AUTHOR_NAME", "tak-test"),
        ("GIT_AUTHOR_EMAIL", "t@example.com"),
        ("GIT_COMMITTER_NAME", "tak-test"),
        ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ];
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.name=tak-test",
        "-c",
        "user.email=t@example.com",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ]);

    let out = p.run_env(
        &["--no-progress", "--record", "--export-json", "r.json"],
        &ident,
    );
    assert!(!out.status.success(), "a failed check must stop --record");
    let err = stderr(&out);
    assert!(err.contains("not recording"), "{err}");
    assert!(err.contains("cmp (bad) failed 2 of 2"), "{err}");
    assert!(!err.contains("cmp (good) failed"), "{err}");
    assert!(
        git(&["notes", "--ref=tak", "list"]).is_empty(),
        "nothing recorded"
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    assert_eq!(
        json["results"].as_array().unwrap().len(),
        2,
        "export written"
    );

    let out = p.run(&["--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));

    // With every check passing, the same project records.
    let out = p.run_env(&["--no-progress", "--record", "--subject", "good"], &ident);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!git(&["notes", "--ref=tak", "list"]).is_empty());
}

/// A step that leaves something running in the background which holds its
/// stderr open. `sleep 30 >&2 &` outlives the step by far more than the run
/// is allowed to take, so a tak waiting for stderr to close would hang here.
const LEAVES_STDERR_OPEN: &str = r#"["sh", "-c", "sleep 30 >&2 & exit 0"]"#;

/// Runs a one-subject benchmark with `step` set to [`LEAVES_STDERR_OPEN`],
/// returning tak's output once it finishes well before the sleep would.
fn run_with_background_step(name: &str, step: &str) -> Output {
    let p = Project::new(
        name,
        &format!(
            "[bench.cmp]\nwarmup = 0\nruns = 1\n[bench.cmp.subject.a]\ncmd = [\"sh\", \"-c\", \"echo run:a >> log\"]\n{step} = {LEAVES_STDERR_OPEN}\n"
        ),
    );
    let start = std::time::Instant::now();
    let out = p.run(&["--no-progress"]);
    let took = start.elapsed();
    assert!(
        took < std::time::Duration::from_secs(10),
        "{step} with a background process took {took:?}: {}",
        stderr(&out)
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.log(), ["run:a"]);
    out
}

/// A prepare whose background process keeps its stderr open is done when the
/// prepare itself exits.
#[test]
fn a_prepare_leaving_stderr_open_does_not_hang() {
    run_with_background_step("bg-prepare", "prepare");
}

/// Likewise a setup — which tak does not kill, since starting a fixture
/// server is a reasonable thing for one to do.
#[test]
fn a_setup_leaving_stderr_open_does_not_hang() {
    run_with_background_step("bg-setup", "setup");
}

/// Likewise a check, which passes on its exit status.
#[test]
fn a_check_leaving_stderr_open_does_not_hang() {
    let out = run_with_background_step("bg-check", "check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("n=1  checks 1/1"), "{stdout}");
}

/// A failing step that backgrounds a process still reports what it wrote to
/// stderr before exiting.
#[test]
fn a_failing_step_leaving_stderr_open_keeps_its_message() {
    let p = Project::new(
        "bg-fails",
        &format!(
            "[bench.cmp]\nwarmup = 0\n{}\n[bench.cmp.subject.broken]\ncmd = [\"sh\", \"-c\", \"echo run:broken >> log\"]\nsetup = [\"sh\", \"-c\", \"echo no fixture >&2; sleep 30 >&2 & exit 1\"]\n",
            logging_subject("a", 1)
        ),
    );
    let start = std::time::Instant::now();
    let out = p.run(&["--no-progress"]);
    let took = start.elapsed();
    assert!(took < std::time::Duration::from_secs(10), "took {took:?}");
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("cmp (broken) dropped")
            && err.contains("setup `sh` exited with")
            && err.contains(": no fixture"),
        "{err}"
    );
}

/// A subject that exits 1 by design, like pre-commit after a hook modified
/// files, is kept when `ok_exit_codes` allows it, warmups included, and its
/// real exit codes are exported. Without the setting the same command is
/// dropped, and a code the list leaves out still drops it.
#[test]
fn ok_exit_codes_keep_a_subject_that_exits_non_zero_by_design() {
    let p = Project::new(
        "ok-exit",
        r#"
[bench.cmp]
runs = 3
warmup = 1

[bench.cmp.subject.allowed]
cmd = ["sh", "-c", "echo run:allowed >> log; exit 1"]
ok_exit_codes = [0, 1]

[bench.cmp.subject.default]
cmd = ["sh", "-c", "exit 1"]

[bench.cmp.subject.other]
cmd = ["sh", "-c", "exit 2"]
ok_exit_codes = [0, 1]
"#,
    );
    let out = p.run(&["--export-json", "results.json", "--no-progress"]);
    assert!(!out.status.success(), "two subjects failed");
    let err = stderr(&out);
    assert!(
        err.contains("cmp (default) dropped") && err.contains("exited with exit status: 1"),
        "{err}"
    );
    assert!(
        err.contains("cmp (other) dropped")
            && err.contains("exit status: 2, and ok_exit_codes is 0, 1"),
        "{err}"
    );
    assert!(!err.contains("cmp (allowed) dropped"), "{err}");

    let runs = p.log().iter().filter(|l| *l == "run:allowed").count();
    assert_eq!(runs, 1 + 3, "the warmup and every timed run");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("results.json")).unwrap()).unwrap();
    let results = json["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["subject"], "allowed");
    assert_eq!(results[0]["exit_codes"], serde_json::json!([1, 1, 1]));
}

/// `ok_exit_codes` describes the program being measured, not its reset: a
/// prepare step that fails still drops the subject.
#[test]
fn ok_exit_codes_do_not_excuse_a_failing_prepare() {
    let p = Project::new(
        "ok-exit-prepare",
        r#"
[bench.one]
cmd = ["sh", "-c", "exit 1"]
prepare = ["sh", "-c", "exit 1"]
ok_exit_codes = [0, 1]
runs = 1
warmup = 0
"#,
    );
    let out = p.run(&["--no-progress"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("prepare"), "{}", stderr(&out));
}

/// A single-command benchmark takes the setting too, and it may leave 0 out:
/// `grep` for something that must not be there.
#[test]
fn ok_exit_codes_may_require_a_non_zero_code() {
    let p = Project::new(
        "ok-exit-single",
        r#"
[bench.nomatch]
cmd = ["sh", "-c", "exit 1"]
ok_exit_codes = [1]
runs = 2
warmup = 0

[bench.match]
cmd = ["true"]
ok_exit_codes = [1]
runs = 1
warmup = 0
"#,
    );
    let out = p.run(&["--bench", "nomatch", "--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = p.run(&["--bench", "match", "--no-progress"]);
    assert!(!out.status.success(), "exit 0 is not in the list");
}

/// --dry-run shows `ok_exit_codes` only where it differs from the default.
#[test]
fn dry_run_shows_non_default_ok_exit_codes() {
    let p = Project::new(
        "ok-exit-dry-run",
        r#"
[bench.cmp.subject.lenient]
cmd = ["true"]
ok_exit_codes = [1, 0]

[bench.cmp.subject.strict]
cmd = ["true"]
"#,
    );
    let out = p.run(&["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.matches("ok exit").count(), 1, "{stdout}");
    assert!(stdout.contains("ok exit  0, 1"), "{stdout}");
}

/// Like `prepare`, `setup` must exit 0 whatever `ok_exit_codes` allow: it
/// builds the state every sample starts from.
#[test]
fn ok_exit_codes_do_not_excuse_a_failing_setup() {
    let p = Project::new(
        "ok-exit-setup",
        r#"
[bench.one]
cmd = ["sh", "-c", "echo ran >> log; exit 1"]
setup = ["sh", "-c", "exit 1"]
ok_exit_codes = [0, 1]
runs = 1
warmup = 0
"#,
    );
    let out = p.run(&["--no-progress"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("setup"), "{}", stderr(&out));
    assert!(p.log().is_empty(), "no sample ran");
}

/// A portable list may name a Windows code next to Unix ones: on Unix it is
/// warned about, and the run goes on with the codes that can match.
#[test]
fn an_ok_exit_code_impossible_here_is_warned_about() {
    let p = Project::new(
        "ok-exit-portable",
        r#"
[bench.one]
cmd = ["sh", "-c", "exit 1"]
ok_exit_codes = [0, 1, -1073741819]
runs = 1
warmup = 0
"#,
    );
    let out = p.run(&["--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("ok_exit_codes -1073741819 can never match here"),
        "{}",
        stderr(&out)
    );
}

/// A Windows-only benchmark whose codes are all impossible on Unix neither
/// stops the file loading nor blocks another benchmark, because `when` keeps
/// it from running. Selected without a `when`, the same subject fails before
/// its setup or any sample runs, in a dry run too; left out with
/// `--subject`, it is not checked.
#[test]
fn ok_exit_codes_impossible_here_are_only_checked_for_what_runs() {
    let p = Project::new(
        "ok-exit-platform",
        r#"
[bench.windows]
when = 'os == "windows"'
cmd = ["cmd", "/C", "exit 1"]
ok_exit_codes = [-1073741819]

[bench.unix]
cmd = ["sh", "-c", "echo ran:unix >> log"]
runs = 1
warmup = 0

[bench.cmp]
runs = 1
warmup = 0

[bench.cmp.subject.win]
cmd = ["sh", "-c", "echo ran:win >> log"]
setup = ["sh", "-c", "echo setup:win >> log"]
ok_exit_codes = [-1073741819]

[bench.cmp.subject.ok]
cmd = ["sh", "-c", "echo ran:ok >> log"]
"#,
    );

    // (a) The Windows-only benchmark is skipped and the Unix one runs.
    // --no-counters: a single-command benchmark counts instructions, and
    // where valgrind exists that runs the command three more times.
    let out = p.run(&["--bench", "unix", "--no-progress", "--no-counters"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = p.run(&["--bench", "windows", "--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.log(), ["ran:unix"]);

    // (b) Selected on Unix, it fails before setup or any sample.
    for args in [
        &["--bench", "cmp", "--no-progress"][..],
        &["--bench", "cmp", "--dry-run"][..],
    ] {
        let out = p.run(args);
        assert!(!out.status.success(), "{args:?}");
        let err = stderr(&out);
        assert!(
            err.contains("benchmark `cmp`, subject `win`")
                && err.contains("-1073741819 can never match")
                && err.contains("0 to 255"),
            "{err}"
        );
    }
    assert_eq!(p.log(), ["ran:unix"], "nothing of cmp ran");

    // (c) Left out with --subject, it is not checked.
    let out = p.run(&["--bench", "cmp", "--subject", "ok", "--no-progress"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.log(), ["ran:unix", "ran:ok"]);
}

/// `ok_exit_codes` keep a command that exits 1, but its `check` still has
/// to exit 0 to pass: the export shows the kept exit codes and the failed
/// checks side by side.
#[test]
fn ok_exit_codes_do_not_pass_a_failing_check() {
    let p = Project::new(
        "ok-exit-check",
        r#"
[bench.cmp]
runs = 2
warmup = 0

[bench.cmp.subject.lint]
cmd = ["sh", "-c", "exit 1"]
check = ["sh", "-c", "exit 1"]
ok_exit_codes = [0, 1]
"#,
    );
    let out = p.run(&["--no-progress", "--export-json", "r.json"]);
    assert!(
        out.status.success(),
        "a failed check alone doesn't fail the run: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("check failed after 2 of 2"),
        "{}",
        stderr(&out)
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    let r = &json["results"][0];
    assert_eq!(r["exit_codes"], serde_json::json!([1, 1]));
    assert_eq!(r["checks"]["passed"], 0);
}

/// A passing prepare and check that leave a process holding stderr cost a
/// sample nothing extra: tak moves on as soon as each step exits.
#[test]
fn passing_steps_leaving_stderr_open_add_no_delay() {
    let p = Project::new(
        "bg-many",
        &format!(
            "[bench.cmp]\nwarmup = 0\nruns = 10\n[bench.cmp.subject.a]\ncmd = [\"sh\", \"-c\", \"echo run:a >> log\"]\nprepare = {LEAVES_STDERR_OPEN}\ncheck = {LEAVES_STDERR_OPEN}\n"
        ),
    );
    let start = std::time::Instant::now();
    let out = p.run(&["--no-progress"]);
    let took = start.elapsed();
    // 10 prepares and 10 checks, each with a background sleep; waiting even
    // 500 ms after each would take 10 s.
    assert!(
        took < std::time::Duration::from_secs(2),
        "took {took:?}: {}",
        stderr(&out)
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("checks 10/10"));
    assert_eq!(p.log().len(), 10);
}

/// A failing step's message comes from the end of its stderr, however much
/// it wrote first.
#[test]
fn a_failing_step_with_long_stderr_reports_its_last_line() {
    let p = Project::new(
        "long-stderr",
        &format!(
            "[bench.cmp]\nwarmup = 0\n{}\n[bench.cmp.subject.broken]\ncmd = [\"sh\", \"-c\", \"echo run:broken >> log\"]\nsetup = [\"sh\", \"-c\", \"{{ yes filler | head -c 10000; echo; echo final line; }} >&2; exit 1\"]\n",
            logging_subject("a", 1)
        ),
    );
    let out = p.run(&["--no-progress"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("cmp (broken) dropped") && err.contains("setup `sh` exited with"),
        "{err}"
    );
    let line = err
        .lines()
        .find(|l| l.contains("setup `sh` exited with"))
        .unwrap();
    assert!(line.trim_end().ends_with(": final line"), "{line}");
}
