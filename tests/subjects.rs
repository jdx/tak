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
        Command::new(env!("CARGO_BIN_EXE_tak"))
            .arg("run")
            .args(args)
            .current_dir(&self.dir)
            .env("GITHUB_TOKEN", "sentinel-must-not-leak")
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
