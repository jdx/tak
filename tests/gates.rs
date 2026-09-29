//! `tak compare` reads each benchmark's gate from the working tree's `tak.toml`.
//!
//! The gating matrix itself is unit-tested in `compare.rs` and `config.rs`.
//! What those cannot see is the wiring: that the binary finds the file, applies
//! it to series that came out of git notes rather than out of a test fixture,
//! falls back to the global gate for what the file does not mention, and still
//! works with no file at all. Each test builds a two-commit repository with
//! notes attached and drives the real binary against it.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};
use tak_cli::record::Record;

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

fn record(bench: &str, instructions: f64) -> String {
    Record {
        v: tak_cli::record::SCHEMA_VERSION,
        bench: bench.into(),
        tool: "self".into(),
        version: None,
        runner: "test-runner".into(),
        ts: "2026-01-01T00:00:00Z".into(),
        metrics: BTreeMap::from([
            ("instructions".to_string(), instructions),
            ("wall_min_ms".to_string(), 1.0),
        ]),
    }
    .to_line()
    .unwrap()
}

/// A repository whose base commit measured `startup` at 450k instructions and
/// `install` at 100M, and whose head measured both 2% higher. Returns the
/// directory and the base commit.
fn repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "--quiet", "-b", "main"]);
    git(p, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
    let base = git(p, &["rev-parse", "HEAD"]);
    let note = |rev: &str, lines: &[String]| {
        git(
            p,
            &["notes", "--ref", "tak", "add", "-m", &lines.join("\n"), rev],
        );
    };
    note(
        &base,
        &[
            record("startup", 450_000.0),
            record("install", 100_000_000.0),
        ],
    );
    git(p, &["commit", "--quiet", "--allow-empty", "-m", "head"]);
    note(
        "HEAD",
        &[
            record("startup", 459_000.0),
            record("install", 102_000_000.0),
        ],
    );
    (dir, base)
}

fn compare(dir: &Path, base: &str, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tak"))
        .arg("compare")
        .arg(base)
        // Nothing to fetch from: the notes are already local, and a failed
        // refresh falls back to them.
        .args(["--remote", "nowhere"])
        .args(extra)
        .env_remove("TAK_GATE_PCT")
        .env_remove("TAK_GATE_MIN_DELTA")
        .env_remove("TAK_CREDIT")
        .current_dir(dir)
        .output()
        .expect("failed to run tak compare")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// With no `tak.toml`, every series is held to the global gate, exactly as
/// before per-benchmark gates existed.
#[test]
fn without_a_tak_toml_the_global_gate_applies_to_everything() {
    let (dir, base) = repo();
    let out = compare(dir.path(), &base, &[]);
    assert!(!out.status.success(), "{}", stdout(&out));
    let report = stdout(&out);
    assert!(
        report.contains("**2 benchmark(s) above the 1% gate:**"),
        "{report}"
    );
    assert!(!report.contains("| gate |"), "{report}");
    assert!(
        stderr(&out).contains("2 benchmark(s) regressed by more than 1%"),
        "{}",
        stderr(&out)
    );
}

/// The case the feature exists for: a looser gate on the small benchmark lets
/// its 2% through, and the large one still fails at 1%.
#[test]
fn a_benchmark_gate_in_tak_toml_decides_that_series() {
    let (dir, base) = repo();
    std::fs::write(
        dir.path().join("tak.toml"),
        "[bench.startup]\ncmd = \"x\"\ngate = { pct = 5.0 }\n\n[bench.install]\ncmd = \"y\"\n",
    )
    .unwrap();
    let out = compare(dir.path(), &base, &[]);
    let report = stdout(&out);
    assert!(!out.status.success(), "{report}");
    assert!(report.contains("| gate |"), "{report}");
    assert!(report.contains("| **+2.00%** | 5% |"), "{report}");
    assert!(
        report.contains("**1 benchmark(s) above their gate:** `install` +2.00% (gate 1%)"),
        "{report}"
    );
    assert!(
        stderr(&out).contains("1 benchmark(s) regressed beyond their gate"),
        "{}",
        stderr(&out)
    );

    // `--gate-pct` moves the global gate, which is what `install` is held to.
    // It does not override the benchmark that declared its own.
    let out = compare(dir.path(), &base, &["--gate-pct", "3"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("No gated benchmark rose beyond its gate."),
        "{}",
        stdout(&out)
    );
}

/// A report-only benchmark is flagged and never fails, and a benchmark in the
/// notes that `tak.toml` no longer declares falls back to the global gate.
#[test]
fn report_only_and_undeclared_benchmarks() {
    let (dir, base) = repo();
    std::fs::write(
        dir.path().join("tak.toml"),
        "[bench.startup]\ncmd = \"x\"\ngate = { enabled = false }\n",
    )
    .unwrap();
    let out = compare(dir.path(), &base, &[]);
    let report = stdout(&out);
    assert!(
        !out.status.success(),
        "install is undeclared, so 1%: {report}"
    );
    assert!(report.contains("report only (1%)"), "{report}");
    assert!(report.contains("**+2.00%** (not gated)"), "{report}");
    assert!(
        report.contains(
            "**1 report-only benchmark(s) above their gate, not failing:** `startup` +2.00%"
        ),
        "{report}"
    );
    assert!(
        report.contains("**1 benchmark(s) above their gate:** `install`"),
        "{report}"
    );

    // Both report-only: the report still says what rose, and nothing fails.
    std::fs::write(
        dir.path().join("tak.toml"),
        "[bench.startup]\ncmd = \"x\"\ngate = { enabled = false }\n\n\
         [bench.install]\ncmd = \"y\"\ngate = { enabled = false }\n",
    )
    .unwrap();
    let out = compare(dir.path(), &base, &[]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("**2 report-only benchmark(s) above their gate"),
        "{}",
        stdout(&out)
    );
}

/// A floor lets a small benchmark keep a tight percentage: startup rose 9,000
/// instructions, under its 20,000 floor, and install rose 2M, over it.
#[test]
fn a_global_floor_from_tak_toml_applies_to_every_series() {
    let (dir, base) = repo();
    std::fs::write(dir.path().join("tak.toml"), "[gate]\nmin_delta = 20000\n").unwrap();
    let out = compare(dir.path(), &base, &[]);
    let report = stdout(&out);
    assert!(!out.status.success(), "{report}");
    assert!(
        report.contains("**1 benchmark(s) above the 1% gate:** `install` +2.00%"),
        "{report}"
    );
    let out = compare(dir.path(), &base, &["--gate-min-delta", "5000000"]);
    assert!(out.status.success(), "{}", stdout(&out));
}

/// A gate that cannot be read is an error before anything is fetched or
/// compared, not a quiet fallback to the global gate.
#[test]
fn a_bad_gate_fails_the_comparison() {
    let (dir, base) = repo();
    std::fs::write(
        dir.path().join("tak.toml"),
        "[bench.startup]\ncmd = \"x\"\ngate = { pct = -5.0 }\n",
    )
    .unwrap();
    let out = compare(dir.path(), &base, &["--no-gate"]);
    assert!(!out.status.success());
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("bench.startup.gate.pct"),
        "{}",
        stderr(&out)
    );

    let out = compare(dir.path(), &base, &["--gate-pct=-1"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("gate percentage"), "{}", stderr(&out));
}
