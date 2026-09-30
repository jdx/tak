//! `tak compare` reads each benchmark's gate from the base revision's `tak.toml`.
//!
//! The gating matrix itself is unit-tested in `compare.rs` and `config.rs`.
//! What those cannot see is the wiring: that the binary finds the file in the
//! base's tree, applies it to series that came out of git notes rather than out
//! of a test fixture, falls back to the global gate for what the file does not
//! mention, and still works with no file at all — and that nothing the head
//! commits to its own `tak.toml` loosens the gate it is held to. Each test
//! builds a two-commit repository with notes attached and drives the real
//! binary against it.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};
use tak_cli::record::Record;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=tak-test", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false"])
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
/// `install` at 100M, and whose head measured both 2% higher, with no
/// `tak.toml` on either. Returns the directory and the base commit.
fn repo() -> (tempfile::TempDir, String) {
    repo_with(None, None, "head")
}

/// A repository whose base commit carries `tak.toml` as `base_toml`, since
/// that is the file `tak compare` gates with.
fn gated(base_toml: &str) -> (tempfile::TempDir, String) {
    repo_with(Some(base_toml), Some(base_toml), "head")
}

/// The same measurements, with `tak.toml` committed as `base_toml` on the base
/// and as `head_toml` on a head committed with `message`. `None` means no file
/// on that side, so `Some` then `None` deletes it.
fn repo_with(
    base_toml: Option<&str>,
    head_toml: Option<&str>,
    message: &str,
) -> (tempfile::TempDir, String) {
    let set = |p: &Path, contents: Option<&str>| {
        let toml = p.join("tak.toml");
        match contents {
            Some(text) => std::fs::write(&toml, text).unwrap(),
            None => {
                let _ = std::fs::remove_file(&toml);
            }
        }
    };
    repo_from(|p| set(p, base_toml), |p| set(p, head_toml), message)
}

/// The same measurements, with the base's files laid out by `base` and the
/// head's by `head`: for trees that are more than one `tak.toml`.
fn repo_from(
    base: impl Fn(&Path),
    head: impl Fn(&Path),
    message: &str,
) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "--quiet", "-b", "main"]);
    base(p);
    git(p, &["add", "--all"]);
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
    head(p);
    git(p, &["add", "--all"]);
    git(p, &["commit", "--quiet", "--allow-empty", "-m", message]);
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
        .env_remove("TAK_ACCEPT_TRAILERS")
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
    let (dir, base) = gated(
        "[bench.startup]\ncmd = \"x\"\ngate = { pct = 5.0 }\n\n[bench.install]\ncmd = \"y\"\n",
    );
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
    let (dir, base) = gated("[bench.startup]\ncmd = \"x\"\ngate = { enabled = false }\n");
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
    let (dir, base) = gated(
        "[bench.startup]\ncmd = \"x\"\ngate = { enabled = false }\n\n\
         [bench.install]\ncmd = \"y\"\ngate = { enabled = false }\n",
    );
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
    let (dir, base) = gated("[gate]\nmin_delta = 20000\n");
    let out = compare(dir.path(), &base, &[]);
    let report = stdout(&out);
    assert!(!out.status.success(), "{report}");
    assert!(
        report.contains("**1 benchmark(s) above the 1% gate (rises of 20,000 instructions or fewer are not counted):** `install` +2.00%"),
        "{report}"
    );
    assert!(
        stderr(&out).contains("1 benchmark(s) regressed by more than 1% and 20000 instructions"),
        "{}",
        stderr(&out)
    );
    let out = compare(dir.path(), &base, &["--gate-min-delta", "5000000"]);
    assert!(out.status.success(), "{}", stdout(&out));
}

/// `tak run` checks the global gate too, from every source, before measuring
/// anything: a bad `TAK_GATE_PCT` in CI should not wait for the comparison
/// after a long run to be found.
#[test]
fn a_bad_global_gate_fails_a_run_before_it_measures() {
    let dir = tempfile::tempdir().unwrap();
    let run = |toml: Option<&str>, flags: &[&str], env: &[(&str, &str)]| {
        if let Some(t) = toml {
            std::fs::write(dir.path().join("tak.toml"), t).unwrap();
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tak"));
        cmd.arg("run")
            .args(flags)
            .env_remove("TAK_GATE_PCT")
            .env_remove("TAK_GATE_MIN_DELTA")
            .envs(env.iter().copied())
            .current_dir(dir.path());
        cmd.output().expect("failed to run tak run")
    };
    let adhoc = [
        "--no-counters",
        "--runs",
        "1",
        "--warmup",
        "0",
        "--",
        "true",
    ];

    // The same run succeeds with a good gate, so the failures below are the
    // gate's and nothing else's.
    let ok = run(None, &adhoc, &[]);
    assert!(ok.status.success(), "{}", stderr(&ok));

    for (label, out) in [
        ("TAK_GATE_PCT", run(None, &adhoc, &[("TAK_GATE_PCT", "-1")])),
        (
            "--gate-pct",
            run(None, &[&["--gate-pct=nan"], &adhoc[..]].concat(), &[]),
        ),
        (
            "[gate] in tak.toml",
            run(
                Some("[gate]\npct = -1.0\n\n[bench.a]\ncmd = \"true\"\n"),
                &["--no-counters", "--runs", "1", "--warmup", "0"],
                &[],
            ),
        ),
    ] {
        assert!(!out.status.success(), "{label}: {}", stdout(&out));
        assert!(
            stdout(&out).is_empty(),
            "{label} measured: {}",
            stdout(&out)
        );
        assert!(
            stderr(&out).contains("gate percentage"),
            "{label}: {}",
            stderr(&out)
        );
    }
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

/// The strict gate every loosening below is tried against: 1%, both
/// benchmarks declared, trailers off.
const STRICT: &str =
    "[gate]\npct = 1.0\n\n[bench.startup]\ncmd = \"x\"\n\n[bench.install]\ncmd = \"y\"\n";

/// A `[gate] pct` of 50%, which lets both 2% rises through.
const LOOSE: &str =
    "[gate]\npct = 50.0\n\n[bench.startup]\ncmd = \"x\"\n\n[bench.install]\ncmd = \"y\"\n";

/// The start of the note saying the head's own policy applies once merged.
const LATER: &str = "This revision changes the gate policy";

/// The whole note, naming what changed.
fn later(changed: &str) -> String {
    format!("{LATER} ({changed}), and the change takes effect once it is merged.")
}

/// The hole this closes: every way a head could loosen its own gate in
/// `tak.toml`, committed as part of the change, and the 2% rises still fail.
/// The note names what the head changed, by effective value.
#[test]
fn a_head_cannot_loosen_the_gate_it_is_held_to() {
    let trailer = "head\n\nTak-Accept: startup\nTak-Accept: install";
    for (label, head, message, changed) in [
        ("a higher [gate] pct", Some(LOOSE), "head", Some("`[gate]`")),
        (
            "report-only benchmarks",
            Some(
                "[bench.startup]\ncmd = \"x\"\ngate = { enabled = false }\n\n\
                 [bench.install]\ncmd = \"y\"\ngate = { enabled = false }\n",
            ),
            "head",
            Some("`install`, `startup`"),
        ),
        (
            "per-benchmark gates",
            Some(
                "[bench.startup]\ncmd = \"x\"\ngate = { pct = 50.0 }\n\n\
                 [bench.install]\ncmd = \"y\"\ngate = { min_delta = 5000000 }\n",
            ),
            "head",
            Some("`install`, `startup`"),
        ),
        (
            "trailers turned on, with a trailer",
            Some("[gate]\naccept_trailers = true\n"),
            trailer,
            Some("`accept_trailers`"),
        ),
        // Every series is at 1% either way, so there is nothing to announce.
        ("tak.toml deleted", None, "head", None),
    ] {
        let (dir, base) = repo_with(Some(STRICT), head, message);
        let out = compare(dir.path(), &base, &[]);
        let report = stdout(&out);
        assert!(!out.status.success(), "{label} loosened the gate: {report}");
        assert!(
            report.contains("**2 benchmark(s) above the 1% gate:**"),
            "{label}: {report}"
        );
        // The note sits below the table: scripts read the first line.
        assert!(report.starts_with('|'), "{label}: {report}");
        match changed {
            Some(changed) => assert!(
                report.contains(&format!(
                    "The gate comes from `tak.toml` at the base, `{}`. {}",
                    &base[..12],
                    later(changed)
                )),
                "{label}: {report}"
            ),
            None => assert!(!report.contains(LATER), "{label}: {report}"),
        }
        if message == trailer {
            assert!(report.contains("not honoured"), "{label}: {report}");
        }
    }
}

/// A benchmark added without a `gate` is held to `[gate]` on both sides, so
/// adding one changes no gate and gets no note.
#[test]
fn adding_a_benchmark_without_a_gate_is_not_a_policy_change() {
    let head = format!("{STRICT}\n[bench.new]\ncmd = \"z\"\n");
    let (dir, base) = repo_with(Some(STRICT), Some(&head), "head");
    let report = stdout(&compare(dir.path(), &base, &[]));
    assert!(!report.contains("at the base"), "{report}");
    assert!(report.starts_with('|'), "{report}");
}

/// A `tak.toml` committed as a symlink is followed inside the base's tree,
/// as the search on disk follows it, and never out of it.
#[cfg(unix)]
#[test]
fn a_symlinked_base_tak_toml_is_followed_inside_the_tree() {
    use std::os::unix::fs::symlink;
    let link = |target: &'static str| {
        move |p: &Path| {
            std::fs::create_dir_all(p.join("config")).unwrap();
            std::fs::write(p.join("config/tak.toml"), STRICT).unwrap();
            symlink(target, p.join("tak.toml")).unwrap();
        }
    };

    // A valid link: the file it names gates, whatever the head's says.
    let (dir, base) = repo_from(
        link("config/tak.toml"),
        |p: &Path| {
            std::fs::remove_file(p.join("tak.toml")).unwrap();
            std::fs::write(p.join("tak.toml"), LOOSE).unwrap();
        },
        "head",
    );
    let out = compare(dir.path(), &base, &[]);
    let report = stdout(&out);
    assert!(!out.status.success(), "{report}");
    assert!(
        report.contains("**2 benchmark(s) above the 1% gate:**"),
        "{report}"
    );
    assert!(report.contains(&later("`[gate]`")), "{report}");

    // Escaping the repository, or pointing at nothing, is an error naming the
    // base rather than a fallback to the defaults.
    for (target, why) in [
        ("../outside.toml", "outside the repository"),
        ("/etc/tak.toml", "outside the repository"),
        ("config/missing.toml", "does not exist there"),
    ] {
        let (dir, base) = repo_from(link(target), |_: &Path| {}, "head");
        let out = compare(dir.path(), &base, &["--no-gate"]);
        assert!(!out.status.success(), "{target}: {}", stdout(&out));
        assert!(stdout(&out).is_empty(), "{target}: {}", stdout(&out));
        let err = stderr(&out);
        assert!(
            err.contains(&format!(
                "cannot read the gate policy from the base, {}",
                &base[..12]
            )),
            "{target}: {err}"
        );
        assert!(err.contains(why), "{target}: {err}");
    }
}

/// A head that changes nothing about the gate gets no note, so the report
/// reads exactly as it did before the gate moved to the base.
#[test]
fn an_unchanged_gate_adds_nothing_to_the_report() {
    let (dir, base) = gated(STRICT);
    let out = compare(dir.path(), &base, &[]);
    let report = stdout(&out);
    assert!(!out.status.success(), "{report}");
    assert!(!report.contains("at the base"), "{report}");
    assert!(report.starts_with('|'), "{report}");
}

/// With no `tak.toml` at the base, the defaults decide, never the head's
/// file, and the report says both.
#[test]
fn without_a_tak_toml_at_the_base_the_defaults_apply() {
    let (dir, base) = repo_with(
        None,
        Some("[bench.startup]\ncmd = \"x\"\ngate = { enabled = false }\n"),
        "head",
    );
    let out = compare(dir.path(), &base, &[]);
    let report = stdout(&out);
    assert!(!out.status.success(), "{report}");
    assert!(
        report.contains("**2 benchmark(s) above the 1% gate:**"),
        "{report}"
    );
    assert!(
        report.contains(&format!(
            "No `tak.toml` at the base, `{}`, so the gate is tak's defaults plus any \
             flags and environment variables. {}",
            &base[..12],
            later("`startup`")
        )),
        "{report}"
    );

    // Nothing on either side: the one line, and no claim that anything changed.
    let (dir, base) = repo();
    let report = stdout(&compare(dir.path(), &base, &[]));
    assert!(report.contains("No `tak.toml` at the base"), "{report}");
    assert!(!report.contains(LATER), "{report}");
}

/// The flags and variables a workflow sets still override the base's file,
/// as they did the working tree's.
#[test]
fn flags_and_the_environment_still_override_the_base() {
    let (dir, base) = repo_with(
        Some(STRICT),
        Some(LOOSE),
        "head\n\nTak-Accept: startup\nTak-Accept: install",
    );
    let out = compare(dir.path(), &base, &["--gate-pct", "3"]);
    assert!(out.status.success(), "--gate-pct: {}", stdout(&out));
    // Both sides resolve under the flag, so neither file's `pct` counts, and
    // merging the head would change nothing while the workflow passes it.
    assert!(!stdout(&out).contains(LATER), "{}", stdout(&out));

    for flags in [
        &["--gate-min-delta", "5000000"][..],
        &["--accept", "startup", "--accept", "install"],
        &["--no-gate"],
    ] {
        let out = compare(dir.path(), &base, flags);
        assert!(out.status.success(), "{flags:?}: {}", stdout(&out));
    }

    let with = |key: &str, value: &str| {
        Command::new(env!("CARGO_BIN_EXE_tak"))
            .args(["compare", &base, "--remote", "nowhere"])
            .env_remove("TAK_GATE_PCT")
            .env_remove("TAK_GATE_MIN_DELTA")
            .env_remove("TAK_ACCEPT_TRAILERS")
            .env(key, value)
            .current_dir(dir.path())
            .output()
            .expect("failed to run tak compare")
    };
    let out = with("TAK_GATE_PCT", "3");
    assert!(out.status.success(), "TAK_GATE_PCT: {}", stdout(&out));
    let out = with("TAK_ACCEPT_TRAILERS", "1");
    assert!(
        out.status.success(),
        "TAK_ACCEPT_TRAILERS: {}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("**2 accepted regression(s)"),
        "{}",
        stdout(&out)
    );
}

/// The base's file is found by searching up from the working directory
/// through the base's tree. A `tak.toml` the head puts nearer the working
/// directory is the one `tak run` would read there, and it still does not
/// gate: the base has nothing at that path, and its root file decides.
#[test]
fn a_head_file_nearer_the_working_directory_does_not_gate() {
    let (dir, base) = gated(STRICT);
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("tak.toml"), LOOSE).unwrap();
    let out = compare(&sub, &base, &[]);
    let report = stdout(&out);
    assert!(!out.status.success(), "{report}");
    assert!(
        report.contains("The gate comes from `tak.toml` at the base"),
        "{report}"
    );
}

/// A base `tak.toml` that does not parse fails the comparison and names the
/// base, rather than falling back to defaults the base never asked for.
#[test]
fn a_base_tak_toml_that_does_not_parse_fails() {
    for broken in [
        "[gate\npct = 1.0\n",
        "[bench.startup]\ncmd = \"x\"\ngate = { pct = -5.0 }\n",
    ] {
        let (dir, base) = repo_with(Some(broken), Some(STRICT), "head");
        let out = compare(dir.path(), &base, &["--no-gate"]);
        assert!(!out.status.success(), "{broken}: {}", stdout(&out));
        assert!(stdout(&out).is_empty(), "{}", stdout(&out));
        let err = stderr(&out);
        assert!(
            err.contains(&format!("at the base, {}", &base[..12])),
            "{err}"
        );
        assert!(err.contains("fix it there"), "{err}");
    }
}

/// A clone holding the base commit but not its tree cannot say what the base
/// gate is, and must not take that for "no tak.toml" and fall back.
#[test]
fn a_base_whose_tree_is_missing_fails() {
    // Different files on each side, so the base's root tree is its own object.
    let (dir, base) = repo_with(Some(STRICT), Some(LOOSE), "head");
    let p = dir.path();
    let tree = git(p, &["rev-parse", &format!("{base}^{{tree}}")]);
    let object = p.join(".git/objects").join(&tree[..2]).join(&tree[2..]);
    std::fs::remove_file(object).unwrap();
    let out = compare(p, &base, &["--no-gate"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    let err = stderr(&out);
    assert!(
        err.contains("cannot read the gate policy from the base"),
        "{err}"
    );
    assert!(err.contains("must be in this clone"), "{err}");
}
