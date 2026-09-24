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
    assert_eq!(json["seed"], 77);
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
