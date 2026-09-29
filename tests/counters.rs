//! Integration tests for the deterministic-metric path.
//!
//! `measure::instructions` was written, unit-tested at the parser level, and
//! then shipped without the cachegrind call ever executing once — valgrind was
//! simply absent from the machine it was developed on. These tests exist so CI
//! exercises the real subprocess, and so the determinism claim the whole project
//! rests on is asserted rather than assumed.
//!
//! Every test skips cleanly when valgrind is unavailable, because that is the
//! normal state on macOS and Windows.

use tak_cli::measure::{self, Plan};
use tak_cli::settings::Settings;

use tak_cli::measure::valgrind_available;

/// A command that exists on the host and does a trivial, fixed amount of work.
///
/// Absolute path on unix so no shell or PATH lookup is involved; on Windows
/// there is no `/bin/echo`, and no cachegrind either, so the counter tests skip
/// and only the wall-clock test needs a working subject.
fn subject() -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["/bin/echo".to_string(), "tak".to_string()]
    }
    #[cfg(windows)]
    {
        vec!["cmd".to_string(), "/C".to_string(), "echo tak".to_string()]
    }
}

#[test]
fn instructions_are_reported_when_valgrind_exists() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let c = measure::instructions(&subject(), None, &Settings::default())
        .expect("cachegrind invocation failed")
        .expect("valgrind present but no I refs parsed");

    // A dynamically linked process cannot retire a trivially small number of
    // instructions; anything tiny means we parsed the wrong thing.
    assert!(
        c.min > 10_000,
        "implausibly low instruction count: {}",
        c.min
    );
    assert!(c.max >= c.min);
    assert!(
        c.runs >= 2,
        "a single sample cannot detect a varying subject"
    );
}

/// The claim the CI gate depends on: repeated runs of an identical command
/// return identical counts.
///
/// Wall clock on the same machine varies by 4-20%; if this metric drifted at
/// all, gating at 1% would be meaningless.
#[test]
fn instruction_counts_are_deterministic() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let cmd = subject();
    // `instructions` already samples repeatedly; doing it again covers variation
    // across separate invocations too, not just within one.
    let outer: Vec<_> = (0..2)
        .map(|_| {
            measure::instructions(&cmd, None, &Settings::default())
                .expect("cachegrind invocation failed")
                .expect("no I refs parsed")
        })
        .collect();

    let min = outer.iter().map(|c| c.min).min().unwrap();
    let max = outer.iter().map(|c| c.max).max().unwrap();
    let spread = (max - min) as f64 / min as f64 * 100.0;

    assert!(
        spread < 0.1,
        "instruction counts varied by {spread:.4}% — the deterministic-gate \
         premise does not hold on this platform"
    );
    // /bin/echo is hermetic, so nothing here should look suspect.
    assert!(outer.iter().all(|c| !c.is_suspect()));
}

/// The two "no counters" outcomes must stay distinguishable: a missing valgrind
/// is `Ok(None)`, while valgrind failing mid-measurement is an error. Reporting
/// the latter as the former sends people installing what they already have.
#[test]
fn availability_and_failure_are_distinct() {
    if valgrind_available() {
        // A command that cannot be spawned makes cachegrind fail rather than
        // vanish, so this must be an error rather than Ok(None).
        let bogus = vec!["/nonexistent/tak-not-a-real-binary".to_string()];
        assert!(
            measure::instructions(&bogus, None, &Settings::default()).is_err(),
            "a failed measurement must not look like a missing valgrind"
        );
    } else {
        assert!(
            measure::instructions(&subject(), None, &Settings::default())
                .unwrap()
                .is_none()
        );
    }
}

/// A subject that starts and then exits unsuccessfully still emits an instruction
/// summary. Checking only for that summary records a crash as an improvement.
#[test]
fn a_nonzero_subject_is_not_counted() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "exit 42".to_string(),
    ];
    let err = measure::instructions(&cmd, None, &Settings::default()).unwrap_err();
    assert!(format!("{err:#}").contains("exited with"), "{err:#}");
}

/// Absence of valgrind must degrade to timing-only, never fail the run.
#[test]
fn missing_valgrind_is_not_an_error() {
    if valgrind_available() {
        eprintln!("skipping: valgrind is installed, cannot test its absence");
        return;
    }
    let got = measure::instructions(&subject(), None, &Settings::default())
        .expect("must not error when valgrind is absent");
    assert!(got.is_none());
}

/// Wall-clock measurement works everywhere, with or without counters.
#[test]
fn wall_clock_works_without_counters() {
    let m = measure::wall(&Plan {
        cmd: subject(),
        warmup: 1,
        runs: 3,
        dir: None,
        settings: Settings::default(),
    })
    .expect("wall measurement failed");

    assert_eq!(m["wall_n"], 3.0);
    assert!(m["wall_min_ms"] <= m["wall_p50_ms"]);
    assert!(m["wall_p50_ms"] <= m["wall_max_ms"]);
    assert!(m["wall_min_ms"] > 0.0);
}

/// A declared subject's prepare step runs before every cachegrind run, and its
/// env reaches the subject under valgrind — each counted run has to start from
/// the same state the timed samples did. Its check does not run.
#[cfg(unix)]
#[test]
fn a_subject_is_prepared_before_every_counted_run() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tak-counters-prep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let s = tak_cli::config::Subject {
        name: "x".into(),
        cmd: ["/bin/sh", "-c", "echo \"run:$MARK\" >> log"]
            .map(String::from)
            .to_vec(),
        prepare: Some(
            ["/bin/sh", "-c", "echo prep >> log"]
                .map(String::from)
                .to_vec(),
        ),
        setup: None,
        setup_dir: None,
        // Never run under cachegrind: those runs are not samples anyone
        // reports, so the log below has no `check` line.
        check: Some(
            ["/bin/sh", "-c", "echo check >> log"]
                .map(String::from)
                .to_vec(),
        ),
        dir: Some(dir.clone()),
        version_cmd: None,
        env: [("MARK".to_string(), "set".to_string())].into(),
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
        allocations: false,
        ok_exit_codes: vec![0],
    };
    let c = measure::subject_instructions(&s, &Settings::default())
        .expect("cachegrind invocation failed")
        .expect("valgrind present but no I refs parsed");
    assert!(
        c.min > 10_000,
        "implausibly low instruction count: {}",
        c.min
    );

    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    let expected = "prep\nrun:set\n".repeat(c.runs as usize);
    assert_eq!(log, expected);
    std::fs::remove_dir_all(&dir).ok();
}

/// `ok_exit_codes` reaches the cachegrind run: a subject that exits 1 by
/// design is counted when it allows 1, and still fails when it does not.
/// cachegrind exits with its client's code, which is what makes this work.
#[cfg(unix)]
#[test]
fn ok_exit_codes_apply_under_valgrind() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let subject = |ok: Vec<i32>| tak_cli::config::Subject {
        name: "x".into(),
        cmd: ["/bin/sh", "-c", "exit 1"].map(String::from).to_vec(),
        setup: None,
        setup_dir: None,
        prepare: None,
        check: None,
        version_cmd: None,
        dir: None,
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
        allocations: false,
        ok_exit_codes: ok,
    };
    let c = measure::subject_instructions(&subject(vec![0, 1]), &Settings::default())
        .expect("exit 1 is allowed")
        .expect("valgrind present but no I refs parsed");
    assert!(c.min > 10_000, "implausibly low: {}", c.min);

    let err = measure::subject_instructions(&subject(vec![0]), &Settings::default()).unwrap_err();
    assert!(format!("{err:#}").contains("under valgrind"), "{err:#}");
}

/// A declared subject that counts heap allocations and nothing else, with
/// the given command.
#[cfg(unix)]
fn allocating(cmd: &[&str]) -> tak_cli::config::Subject {
    tak_cli::config::Subject {
        name: "x".into(),
        cmd: cmd.iter().map(|s| s.to_string()).collect(),
        setup: None,
        setup_dir: None,
        prepare: None,
        check: None,
        version_cmd: None,
        dir: None,
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
        counters: false,
        allocations: true,
        ok_exit_codes: vec![0],
    }
}

/// The real DHAT subprocess runs and its summary is parsed. A dynamically
/// linked `echo` allocates a few blocks for its locale and stdout buffer;
/// none at all would mean the summary was misread, or DHAT saw nothing.
#[cfg(unix)]
#[test]
fn allocations_are_reported_when_valgrind_exists() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let a = measure::subject_allocations(&allocating(&["/bin/echo", "tak"]), &Settings::default())
        .expect("DHAT invocation failed")
        .expect("valgrind present but no DHAT summary parsed");
    assert!(a.min.blocks > 0 && a.min.bytes > 0, "{a:?}");
    assert!(!a.saw_nothing());
    assert!(
        a.min.peak_bytes <= a.min.bytes,
        "a peak above the total: {a:?}"
    );
    assert!(
        a.runs >= 2,
        "a single sample cannot detect a varying subject"
    );
}

/// The hypothesis behind recording them: a single-threaded, hermetic
/// command allocates exactly the same on every run, across separate
/// invocations as well as within one.
#[cfg(unix)]
#[test]
fn allocations_of_a_hermetic_command_repeat_exactly() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let s = allocating(&["/bin/echo", "tak"]);
    let outer: Vec<_> = (0..2)
        .map(|_| {
            measure::subject_allocations(&s, &Settings::default())
                .expect("DHAT invocation failed")
                .expect("no DHAT summary parsed")
        })
        .collect();
    for a in &outer {
        assert_eq!(a.min, a.max, "varied within one measurement: {a:?}");
        assert!(!a.is_suspect());
    }
    assert_eq!(outer[0].min, outer[1].min, "varied between measurements");
}

/// DHAT runs get the same prepare step, environment and `ok_exit_codes` as
/// cachegrind ones, and a subject that fails is not recorded as having
/// allocated little.
#[cfg(unix)]
#[test]
fn allocation_runs_are_prepared_and_judged_like_counted_ones() {
    if !valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut s = allocating(&["/bin/sh", "-c", "echo \"run:$MARK\" >> log"]);
    s.prepare = Some(
        ["/bin/sh", "-c", "echo prep >> log"]
            .map(String::from)
            .to_vec(),
    );
    s.dir = Some(dir.path().to_path_buf());
    s.env = [("MARK".to_string(), "set".to_string())].into();
    let a = measure::subject_allocations(&s, &Settings::default())
        .expect("DHAT invocation failed")
        .expect("no DHAT summary parsed");
    let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
    assert_eq!(log, "prep\nrun:set\n".repeat(a.runs as usize));
    assert!(
        std::fs::read_dir(dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("dhat.out")),
        "DHAT left its profile behind in the subject's directory"
    );

    let mut failing = allocating(&["/bin/sh", "-c", "exit 1"]);
    let err = measure::subject_allocations(&failing, &Settings::default()).unwrap_err();
    assert!(format!("{err:#}").contains("under valgrind"), "{err:#}");
    failing.ok_exit_codes = vec![0, 1];
    assert!(
        measure::subject_allocations(&failing, &Settings::default())
            .expect("exit 1 is allowed")
            .is_some()
    );
}

/// End to end: `--allocations` on an ad-hoc command prints the counts and
/// exports them. Without valgrind it says why they are missing, and the
/// export carries no `allocations` key rather than zeros.
#[cfg(unix)]
#[test]
fn the_allocations_flag_reaches_the_output_and_the_export() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_tak"))
        .args([
            "run",
            "--allocations",
            "--no-counters",
            "--no-progress",
            "--runs",
            "2",
            "--warmup",
            "0",
            "--export-json",
            "r.json",
            "--",
            "/bin/echo",
            "tak",
        ])
        .current_dir(dir.path())
        .output()
        .expect("failed to run tak");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    let export: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("r.json")).unwrap()).unwrap();
    let allocations = &export["results"][0]["allocations"];
    if valgrind_available() {
        assert!(stdout.contains("alloc_blocks"), "{stdout}");
        assert!(allocations["blocks"].as_u64().unwrap() > 0, "{export}");
        assert!(allocations["peak_bytes"].is_u64(), "{export}");
    } else {
        assert!(
            stderr.contains("heap allocations were not measured"),
            "{stderr}"
        );
        assert!(allocations.is_null(), "{export}");
    }
}
