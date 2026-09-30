//! `tak compare`'s exit status is the gate, so it is tested on the real binary
//! rather than on `Comparison`: the unit tests already held the report to
//! saying "nothing was compared", and that sentence exited 0 for as long as
//! nobody asserted on the exit code.
//!
//! Notes are written with plain `git notes` rather than `tak run --record`, so
//! the instruction counts are chosen rather than measured and the tests run
//! without valgrind. Without that, the clean-comparison case would compare
//! only wall clock on most hosts, which never gates.

use std::path::Path;
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

fn tak(dir: &Path, args: &[&str]) -> Output {
    tak_env(dir, args, &[])
}

fn tak_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(args)
        .current_dir(dir)
        // A gate percentage exported by the calling environment would change
        // what counts as a regression here, and an exported `allow_empty`
        // would change whether comparing nothing fails.
        .env_remove("TAK_GATE_PCT")
        .env_remove("TAK_ALLOW_EMPTY")
        // The warning's shape depends on it, and CI sets it.
        .env_remove("GITHUB_ACTIONS")
        .envs(env.iter().copied())
        .output()
        .expect("failed to run tak")
}

/// A repository with two commits, `base` and `head`, and no remote: the
/// refresh from `origin` fails and compare falls back to the local notes,
/// which is the path a prefetched CI checkout takes too.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("tak-compare-")
        .tempdir()
        .unwrap();
    git(dir.path(), &["init", "--quiet", "-b", "main"]);
    git(
        dir.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "base"],
    );
    git(dir.path(), &["tag", "base"]);
    git(
        dir.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "head"],
    );
    dir
}

fn note(dir: &Path, rev: &str, runner: &str, instructions: u64) {
    let line = format!(
        r#"{{"bench":"startup","metrics":{{"instructions":{instructions},"wall_min_ms":1.0}},"runner":"{runner}","tool":"tak","ts":"2026-09-29T00:00:00Z","v":1}}"#
    );
    git(
        dir,
        &["notes", "--ref", "tak", "add", "-f", "-m", &line, rev],
    );
}

fn compare(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["compare", "base"];
    args.extend_from_slice(extra);
    tak(dir, &args)
}

/// The two states that produce an empty comparison in practice: a base that
/// was never recorded, and a runner-class migration.
fn empty_repos() -> Vec<(&'static str, tempfile::TempDir)> {
    let unrecorded = repo();
    note(unrecorded.path(), "HEAD", "ci-linux", 1_000_000);

    let migrated = repo();
    note(migrated.path(), "base", "ci-linux-old", 1_000_000);
    note(migrated.path(), "HEAD", "ci-linux-new", 1_000_000);

    vec![
        ("unrecorded base", unrecorded),
        ("runner migration", migrated),
    ]
}

#[test]
fn an_empty_comparison_fails_after_printing_the_report() {
    for (case, dir) in empty_repos() {
        let out = compare(dir.path(), &[]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{case}: passed with nothing compared"
        );
        // Workflows grep for this prefix; it is part of the interface.
        assert!(
            stdout.contains("**Nothing was compared"),
            "{case}: report missing: {stdout}"
        );
        // The report still names what did not line up — the useful half of
        // it when a runner class has just changed.
        assert!(
            stdout.contains("New, nothing to compare against"),
            "{case}: added list missing: {stdout}"
        );
        assert!(stderr.contains("--allow-empty"), "{case}: {stderr}");
    }
}

#[test]
fn allow_empty_and_no_gate_pass_an_empty_comparison() {
    for (case, dir) in empty_repos() {
        for flag in ["--allow-empty", "--no-gate"] {
            let out = compare(dir.path(), &[flag]);
            assert!(
                out.status.success(),
                "{case} with {flag} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("**Nothing was compared"),
                "{case} with {flag}: report missing"
            );
        }
    }
}

#[test]
fn a_clean_comparison_passes() {
    let dir = repo();
    note(dir.path(), "base", "ci-linux", 1_000_000);
    // 0.1%, inside the default 1% gate.
    note(dir.path(), "HEAD", "ci-linux", 1_001_000);
    let out = compare(dir.path(), &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "failed: {}\n{stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!stdout.contains("Nothing was compared"), "{stdout}");
}

/// `--allow-empty` forgives only the empty case. Were it to pass a regression
/// too, a workflow that set it once for a runner migration and forgot it would
/// have silently stopped gating.
#[test]
fn allow_empty_still_fails_a_regression() {
    let dir = repo();
    note(dir.path(), "base", "ci-linux", 1_000_000);
    note(dir.path(), "HEAD", "ci-linux", 1_100_000);
    let out = compare(dir.path(), &["--allow-empty"]);
    assert!(!out.status.success(), "a 10% rise passed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("regressed"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The report's first line when nothing was compared. Scripts classify a run
/// by it — jdx/tak-action matches the whole bold sentence — so it is held here
/// byte for byte, whatever `allow_empty` says.
const NOTHING_COMPARED: &str = "**Nothing was compared, and so nothing was gated.** No series \
     appears on both sides: either the base has no measurements recorded, or the two were \
     measured on different runner classes, which are deliberately not comparable — counts \
     shift between machine types by more than a real regression does.";

/// As [`repo`], with `tak.toml` committed on the base and on the head as
/// given, and notes for a runner-class migration: nothing can be compared.
fn migrated_with(base_toml: Option<&str>, head_toml: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("tak-compare-")
        .tempdir()
        .unwrap();
    let d = dir.path();
    git(d, &["init", "--quiet", "-b", "main"]);
    for (toml, message) in [(base_toml, "base"), (head_toml, "head")] {
        match toml {
            Some(text) => {
                std::fs::write(d.join("tak.toml"), text).unwrap();
                git(d, &["add", "tak.toml"]);
            }
            None if d.join("tak.toml").exists() => {
                git(d, &["rm", "--quiet", "tak.toml"]);
            }
            None => {}
        }
        git(d, &["commit", "--quiet", "--allow-empty", "-m", message]);
        if message == "base" {
            git(d, &["tag", "base"]);
        }
    }
    note(d, "base", "ci-linux-old", 1_000_000);
    note(d, "HEAD", "ci-linux-new", 1_000_000);
    dir
}

const ALLOW: &str = "[gate]\nallow_empty = true\n";

fn out_of(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// On in the base's `tak.toml`, an empty comparison exits 0, with the first
/// line untouched, the setting named on the line below it, and one warning
/// line on stderr. `--allow-empty` gives exactly the same result: it is a
/// spelling of the setting, not a separate waiver.
#[test]
fn allow_empty_in_the_base_passes_with_a_warning() {
    let dir = migrated_with(Some(ALLOW), Some(ALLOW));
    let out = compare(dir.path(), &[]);
    let (stdout, stderr) = out_of(&out);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(stdout.lines().next(), Some(NOTHING_COMPARED), "{stdout}");
    assert!(
        stdout.contains("passes with a warning instead of failing, because `allow_empty` is on"),
        "{stdout}"
    );
    assert_eq!(
        stderr,
        "warning: nothing was compared: no series was measured on both base and HEAD; \
         passing because `allow_empty` is on\n"
    );

    let flagged = migrated_with(None, None);
    let by_flag = compare(flagged.path(), &["--allow-empty"]);
    assert!(by_flag.status.success());
    let (flag_stdout, flag_stderr) = out_of(&by_flag);
    assert_eq!(flag_stderr, stderr);
    // The same report, less the line saying the base has no tak.toml.
    assert_eq!(flag_stdout.lines().next(), Some(NOTHING_COMPARED));
    assert!(
        flag_stdout.contains("passes with a warning instead of failing"),
        "{flag_stdout}"
    );
}

/// The base-read rule. A pull request that turns `allow_empty` on in its own
/// `tak.toml` cannot pass its own empty comparison: the base's file decides,
/// and the report says the change takes effect once merged.
#[test]
fn a_change_cannot_allow_its_own_empty_comparison() {
    for base in [None, Some("[gate]\npct = 1.0\n")] {
        let dir = migrated_with(base, Some(ALLOW));
        let out = compare(dir.path(), &[]);
        let (stdout, stderr) = out_of(&out);
        assert!(
            !out.status.success(),
            "{base:?}: the head's file waived: {stdout}"
        );
        assert_eq!(stdout.lines().next(), Some(NOTHING_COMPARED), "{stdout}");
        assert!(
            stdout.contains(
                "This revision changes the gate policy (`allow_empty`), and the change \
                 takes effect once it is merged."
            ),
            "{base:?}: {stdout}"
        );
        assert!(!stdout.contains("passes with a warning"), "{stdout}");
        assert!(stderr.contains("nothing was compared"), "{stderr}");
        assert!(stderr.contains("[gate] allow_empty = true"), "{stderr}");
    }

    // And the reverse: turning it off on a branch does not fail that branch.
    let dir = migrated_with(Some(ALLOW), None);
    let out = compare(dir.path(), &[]);
    let (stdout, _) = out_of(&out);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains("changes the gate policy (`allow_empty`)"),
        "{stdout}"
    );
}

/// The environment overrides the base's file in both directions, as for every
/// other gate setting, and `--allow-empty` overrides the environment.
#[test]
fn allow_empty_precedence_on_the_command_line() {
    let on = migrated_with(Some(ALLOW), Some(ALLOW));
    let off = tak_env(on.path(), &["compare", "base"], &[("TAK_ALLOW_EMPTY", "0")]);
    assert!(
        !off.status.success(),
        "TAK_ALLOW_EMPTY=0 beats the base's file"
    );
    let flag = tak_env(
        on.path(),
        &["compare", "base", "--allow-empty"],
        &[("TAK_ALLOW_EMPTY", "0")],
    );
    assert!(flag.status.success(), "the flag beats the environment");

    let unset = migrated_with(None, None);
    let env = tak_env(
        unset.path(),
        &["compare", "base"],
        &[("TAK_ALLOW_EMPTY", "1")],
    );
    assert!(env.status.success(), "{}", out_of(&env).1);
}

/// `allow_empty` waives the empty case and nothing else. A regression still
/// fails with it on in the base, and says nothing about the setting.
#[test]
fn allow_empty_in_the_base_still_fails_a_regression() {
    let dir = migrated_with(Some(ALLOW), Some(ALLOW));
    note(dir.path(), "base", "ci-linux", 1_000_000);
    note(dir.path(), "HEAD", "ci-linux", 1_100_000);
    let out = compare(dir.path(), &[]);
    let (stdout, stderr) = out_of(&out);
    assert!(!out.status.success(), "a 10% rise passed: {stdout}");
    assert!(stderr.contains("regressed"), "{stderr}");
    assert!(!stdout.contains("allow_empty"), "{stdout}");
    assert!(!stderr.contains("allow_empty"), "{stderr}");
}

/// `--no-gate` never fails and was asked for, so it gets no warning and the
/// report does not claim `allow_empty` let it through.
#[test]
fn no_gate_wins_over_allow_empty_without_a_warning() {
    let dir = migrated_with(Some(ALLOW), Some(ALLOW));
    let out = compare(dir.path(), &["--no-gate"]);
    let (stdout, stderr) = out_of(&out);
    assert!(out.status.success());
    assert_eq!(stdout.lines().next(), Some(NOTHING_COMPARED), "{stdout}");
    assert!(!stdout.contains("passes with a warning"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
}

/// Under GitHub Actions the warning is a workflow command on stderr, so it
/// shows as an annotation, and the report on stdout — which workflows
/// redirect to a file and classify by its first line — is unchanged.
#[test]
fn allow_empty_warns_as_an_annotation_under_github_actions() {
    let dir = migrated_with(Some(ALLOW), Some(ALLOW));
    let plain = compare(dir.path(), &[]);
    let out = tak_env(
        dir.path(),
        &["compare", "base"],
        &[("GITHUB_ACTIONS", "true")],
    );
    let (stdout, stderr) = out_of(&out);
    assert!(out.status.success());
    assert_eq!(stdout, out_of(&plain).0);
    assert_eq!(
        stderr,
        "::warning::tak: nothing was compared: no series was measured on both base and \
         HEAD; passing because `allow_empty` is on\n"
    );
}
