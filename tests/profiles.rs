//! `tak run --profile-dir` and `tak explain` against real cachegrind output.
//!
//! The parser's unit tests run on small hand-written fixtures, which say what
//! the format should look like. These say what valgrind on this machine
//! actually writes, and that the profile kept is the one behind the count
//! reported — an attribution that does not add up to the recorded number
//! explains some other run.
//!
//! Tests that need valgrind skip without it, as in `tests/counters.rs`.

use std::path::Path;
use std::process::{Command, Output};
use tak_cli::measure::{self, valgrind_available};
use tak_cli::profile::{self, Profile};
use tak_cli::settings::Settings;

fn tak(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run tak")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[cfg(unix)]
fn echo() -> tak_cli::config::Subject {
    tak_cli::config::Subject {
        name: "x".into(),
        cmd: ["/bin/echo", "tak"].map(String::from).to_vec(),
        prepare: None,
        setup: None,
        setup_dir: None,
        check: None,
        dir: None,
        version_cmd: None,
        env: Default::default(),
        vars: Default::default(),
        when: None,
        runs: tak_cli::config::Runs::Fixed(1),
        auto: tak_cli::config::AutoRuns {
            budget: std::time::Duration::from_secs(30),
            min: 5,
            max: 50,
        },
        warmup: 0,
        counters: true,
        ok_exit_codes: vec![0],
    }
}

/// The kept profile is the one whose count was reported: its total is the
/// minimum, and its functions add up to that total.
#[cfg(unix)]
#[test]
fn a_kept_profile_adds_up_to_the_reported_count() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let (counted, raw) = measure::subject_profile(&echo(), &Settings::default())
        .expect("cachegrind invocation failed")
        .expect("valgrind present but nothing counted");
    let raw = raw.expect("the profile was kept");
    let p = profile::parse(&String::from_utf8(raw).unwrap()).expect("cachegrind's own output");

    assert_eq!(p.total, counted.min);
    assert_eq!(p.functions.values().map(|f| f.ir).sum::<u64>(), p.total);
    assert!(
        p.functions.len() > 10,
        "a dynamically linked process runs more than {} functions",
        p.functions.len()
    );
    assert_eq!(p.cmd.as_deref(), Some("/bin/echo tak"));
}

/// The CLI end to end: two runs of the same benchmark doing different work
/// leave profiles at the documented path, and `explain` pairs them and names
/// what changed.
#[cfg(unix)]
#[test]
fn explain_names_the_functions_between_two_runs() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for (side, arg) in [("base", "/"), ("head", "/usr")] {
        let out = tak(
            dir.path(),
            &[
                "run",
                "--no-progress",
                "--runs",
                "1",
                "--warmup",
                "0",
                "--bench",
                "list",
                "--profile-dir",
                side,
                "--",
                "/bin/ls",
                "-l",
                arg,
            ],
        );
        assert!(out.status.success(), "{}", text(&out));
        assert!(text(&out).contains("wrote 1 profile(s)"), "{}", text(&out));
    }
    let kept = dir.path().join("head/list/self.cachegrind.out");
    let p = Profile::load(&kept).unwrap();
    assert!(p.runner().is_some(), "the runner class is recorded");

    let out = tak(dir.path(), &["explain", "base", "head", "--top", "3"]);
    assert!(out.status.success(), "{}", text(&out));
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("### list\n"), "{md}");
    assert!(md.contains("| function | Δ | base | head | file |"), "{md}");
    // Three rows and one for the rest: listing /usr is not the same work as
    // listing /.
    assert!(md.contains(" more | "), "{md}");
}

/// Everything below runs without valgrind.
#[test]
fn explain_reads_two_profile_files() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/profile");
    let out = tak(
        &fixtures,
        &[
            "explain",
            "base.cachegrind.out",
            "head.cachegrind.out",
            "--top",
            "1",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("1,500 → 2,110 instructions"), "{md}");
    assert!(md.contains("| `mycli::render` | **+900** |"), "{md}");
    assert!(md.contains("| 3 more | -290 |"), "{md}");
}

#[test]
fn profiles_need_counters() {
    let dir = tempfile::tempdir().unwrap();
    let out = tak(
        dir.path(),
        &[
            "run",
            "--no-counters",
            "--profile-dir",
            "p",
            "--",
            "/bin/echo",
        ],
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("--no-counters"), "{}", text(&out));
    assert!(!dir.path().join("p").exists());
}

/// A benchmark name that is not a plain file name fails before anything runs,
/// rather than writing outside the profile directory — or, with a newline,
/// writing a heading of its own into the report.
#[test]
fn a_benchmark_name_that_is_not_a_file_name_is_refused_up_front() {
    for bench in ["../escape", "a\n## All clear"] {
        let dir = tempfile::tempdir().unwrap();
        let out = tak(
            dir.path(),
            &[
                "run",
                "--bench",
                bench,
                "--profile-dir",
                "p",
                "--",
                "/bin/sh",
                "-c",
                "touch ran",
            ],
        );
        assert!(!out.status.success(), "{bench:?}");
        assert!(
            text(&out).contains("not a plain file name"),
            "{}",
            text(&out)
        );
        assert!(!dir.path().join("ran").exists(), "nothing was measured");
    }
}

/// cachegrind that cannot write its profile reports zero instructions. A
/// subject that removes the scratch directory must cost the count, loudly,
/// never record as zero.
#[cfg(unix)]
#[test]
fn a_profile_cachegrind_could_not_write_is_not_a_count_of_zero() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("tmp");
    std::fs::create_dir(&tmp).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(["run", "--no-progress", "--runs", "1", "--warmup", "0"])
        .args(["--profile-dir", "p", "--", "/bin/sh", "-c"])
        .arg("rm -rf \"$TMPDIR\"/.tmp*")
        .current_dir(dir.path())
        .env("TMPDIR", &tmp)
        .output()
        .unwrap();
    let all = text(&out);
    assert!(all.contains("could not write its profile"), "{all}");
    assert!(!all.contains("instructions "), "{all}");
    assert!(all.contains("wrote 0 profile(s)"), "{all}");
}

#[cfg(unix)]
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=tak-test", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git failed to spawn");
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
}

/// Measure the same commit twice and the profile left behind is the second
/// run's, while the notes' lowest count is the first's. `explain` says so
/// instead of attributing a count the gate never used.
#[cfg(unix)]
#[test]
fn a_profile_replaced_after_the_recorded_minimum_is_called_out() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "c"]);
    // Less work first, then more, both recorded against the same commit.
    for arg in ["/usr", "/"] {
        let out = tak(
            dir.path(),
            &[
                "run",
                "--no-progress",
                "--runs",
                "1",
                "--warmup",
                "0",
                "--bench",
                "list",
                "--record",
                "--profile-dir",
                "p",
                "--",
                "/bin/ls",
                "-l",
                arg,
            ],
        );
        assert!(out.status.success(), "{}", text(&out));
    }
    let out = tak(dir.path(), &["explain", "p", "p"]);
    assert!(out.status.success(), "{}", text(&out));
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("The base profile totals") && md.contains("The head profile totals"),
        "{md}"
    );
    assert!(md.contains("in git notes is"), "{md}");
}
