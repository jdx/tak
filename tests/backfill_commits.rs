//! `tak backfill --commits`, run against a real repository.
//!
//! Each test builds a tiny git history whose "build" copies a shell script
//! into place, so a commit's measurement can only come from that commit's own
//! checkout: the script prints its version, which `version_cmd` records.
//! Nothing here needs valgrind — without it the records carry timing only,
//! which is all these assertions look at.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tak_cli::record::{Record, parse_note};

/// A scratch repository, and a private temporary directory for tak to put its
/// checkouts in, so a test can see that they were removed.
struct Repo {
    _root: tempfile::TempDir,
    dir: PathBuf,
    tmp: PathBuf,
}

/// Isolated from the user's git configuration: a global hook, signing key or
/// default branch name must not change what these tests see.
fn git_env(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "tak test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "tak test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
}

impl Repo {
    fn new() -> Repo {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("repo");
        let tmp = root.path().join("tmp");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        let r = Repo {
            _root: root,
            dir,
            tmp,
        };
        r.git(&["init", "-q", "-b", "main"]);
        r
    }

    fn git(&self, args: &[&str]) -> String {
        let out = git_env(&mut Command::new("git"))
            .args(args)
            .current_dir(&self.dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write(&self, path: &str, text: &str) {
        let p = self.dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// Commit a `tool.sh` that prints `version`, or remove it when `None`,
    /// which is what makes that commit's build fail.
    fn commit_tool(&self, version: Option<&str>) -> String {
        match version {
            Some(v) => {
                self.write("tool.sh", &format!("#!/bin/sh\necho {v}\n"));
                make_executable(&self.dir.join("tool.sh"));
                self.git(&["add", "tool.sh"]);
            }
            None => {
                self.git(&["rm", "-q", "tool.sh"]);
            }
        }
        self.git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            version.unwrap_or("remove the tool"),
        ]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn tak(&self, args: &[&str]) -> Output {
        git_env(&mut self.tak_cmd(args)).output().unwrap()
    }

    fn tak_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tak"));
        // A fixed class, so the result does not depend on whether the test
        // runs under GitHub Actions.
        cmd.args(["--runner", "test"])
            .arg("backfill")
            .args(args)
            .current_dir(&self.dir)
            .env("TMPDIR", &self.tmp);
        git_env(&mut cmd);
        cmd
    }

    fn notes(&self, sha: &str) -> Vec<Record> {
        let out = git_env(&mut Command::new("git"))
            .args(["notes", "--ref=tak", "show", sha])
            .current_dir(&self.dir)
            .output()
            .unwrap();
        parse_note(&String::from_utf8_lossy(&out.stdout))
    }

    /// Worktrees git knows about, the main one included.
    fn worktrees(&self) -> usize {
        self.git(&["worktree", "list", "--porcelain"])
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count()
    }

    fn tmp_is_empty(&self) -> bool {
        std::fs::read_dir(&self.tmp).unwrap().next().is_none()
    }

    fn builds(&self) -> usize {
        std::fs::read_to_string(self.build_log())
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn build_log(&self) -> PathBuf {
        self.tmp.parent().unwrap().join("builds.log")
    }
}

fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn both(o: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The current tak.toml, written into the working tree and never committed:
/// backfill reads the file from here, whatever each commit held.
///
/// The build logs one line per run to a file outside the repository, so a
/// test can count builds, then copies the script into place. A shell is fine
/// here: the build is not measured.
fn config(repo: &Repo, extra: &str) {
    repo.write(
        "tak.toml",
        &format!(
            r#"
[build]
cmd = ["sh", "-c", "echo built >> {log} && cp tool.sh tool"]

[bench.startup]
cmd = ["./tool"]
version_cmd = ["./tool"]
runs = 2
warmup = 0
{extra}
"#,
            log = repo.build_log().display()
        ),
    );
}

/// v1, v2, a commit whose build fails, then v4.
fn history(repo: &Repo) -> [String; 4] {
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.git(&["add", ".gitignore"]);
    [
        repo.commit_tool(Some("v1")),
        repo.commit_tool(Some("v2")),
        repo.commit_tool(None),
        repo.commit_tool(Some("v4")),
    ]
}

fn versions(repo: &Repo, sha: &str) -> Vec<String> {
    repo.notes(sha)
        .into_iter()
        .map(|r| r.version.unwrap_or_default())
        .collect()
}

#[test]
fn each_commit_is_measured_in_its_own_checkout() {
    let repo = Repo::new();
    config(&repo, "");
    let [v1, v2, broken, v4] = history(&repo);

    // `v1..main` excludes v1 itself, as `git rev-list` does.
    let out = repo.tak(&["--commits", &format!("{v1}..main")]);
    assert!(out.status.success(), "{}", both(&out));

    assert_eq!(versions(&repo, &v4), ["v4"]);
    assert_eq!(versions(&repo, &v2), ["v2"]);
    assert!(
        repo.notes(&broken).is_empty(),
        "a failed build records nothing"
    );
    assert!(repo.notes(&v1).is_empty(), "outside the range");
    assert!(
        stdout(&out).contains(&format!("build failed: {}", &broken[..12])),
        "{}",
        stdout(&out)
    );

    let rec = &repo.notes(&v4)[0];
    assert_eq!(rec.runner, "test");
    assert_eq!(rec.bench, "startup");
    // Optional everywhere else in this file, but where valgrind exists the
    // backfilled points must carry the one metric a gate can use.
    if tak_cli::measure::valgrind_available() {
        assert!(rec.metrics.contains_key("instructions"), "{rec:?}");
    }
    // The commit's own date, in the one shape `ts` is written in.
    let committed = git_env(&mut Command::new("git"))
        .args([
            "show",
            "-s",
            "--format=%cd",
            "--date=format-local:%Y-%m-%dT%H:%M:%SZ",
            &v4,
        ])
        .env("TZ", "UTC")
        .current_dir(&repo.dir)
        .output()
        .unwrap();
    assert_eq!(rec.ts, String::from_utf8_lossy(&committed.stdout).trim());

    assert_eq!(repo.worktrees(), 1, "every checkout is removed");
    assert!(repo.tmp_is_empty(), "and its directory");
}

#[test]
fn recorded_commits_are_skipped_unless_forced() {
    let repo = Repo::new();
    config(&repo, "");
    let [v1, v2, _broken, v4] = history(&repo);
    let range = format!("{v1}..main");

    let first = repo.tak(&["--commits", &range]);
    assert!(first.status.success(), "{}", both(&first));
    assert_eq!(repo.builds(), 3);

    // Only the commit that failed to build is tried again, and a run with
    // nothing new to record still succeeds: the range has its history.
    let again = repo.tak(&["--commits", &range]);
    assert!(again.status.success(), "{}", both(&again));
    assert_eq!(repo.builds(), 4);
    assert!(
        stdout(&again).contains("2 already recorded"),
        "{}",
        stdout(&again)
    );
    assert_eq!(repo.notes(&v4).len(), 1);
    assert_eq!(repo.notes(&v2).len(), 1);

    let forced = repo.tak(&["--commits", &range, "--force"]);
    assert!(forced.status.success(), "{}", both(&forced));
    assert_eq!(repo.builds(), 7);
    assert_eq!(versions(&repo, &v4), ["v4", "v4"]);
    assert_eq!(repo.notes(&v2).len(), 2);
}

/// Records from another runner class are a different series, and do not
/// make a commit count as done for this one.
#[test]
fn another_runner_class_does_not_count_as_recorded() {
    let repo = Repo::new();
    config(&repo, "");
    let [v1, _, _, v4] = history(&repo);
    let range = format!("{v1}..main");

    assert!(repo.tak(&["--commits", &range]).status.success());
    // The later of two `--runner` flags wins.
    let other = repo
        .tak_cmd(&["--commits", &range, "--runner", "other"])
        .output()
        .unwrap();
    assert!(other.status.success(), "{}", both(&other));
    let mut runners: Vec<String> = repo.notes(&v4).into_iter().map(|r| r.runner).collect();
    runners.sort();
    assert_eq!(runners, ["other", "test"]);
}

#[test]
fn a_dry_run_lists_and_builds_nothing() {
    let repo = Repo::new();
    config(&repo, "");
    let [v1, v2, broken, v4] = history(&repo);

    let out = repo.tak(&["--commits", &format!("{v1}..main"), "--dry-run"]);
    assert!(out.status.success(), "{}", both(&out));
    let text = stdout(&out);
    for sha in [&v2, &broken, &v4] {
        assert!(
            text.contains(&format!("{}  would build", &sha[..12])),
            "{text}"
        );
    }
    assert_eq!(repo.builds(), 0);
    assert!(repo.notes(&v4).is_empty());
    assert_eq!(repo.worktrees(), 1);
}

/// `--limit` caps builds, newest first, so running the same command again
/// continues with the commits the last run did not reach.
#[test]
fn the_limit_counts_builds_newest_first() {
    let repo = Repo::new();
    config(&repo, "");
    let [v1, v2, broken, v4] = history(&repo);
    let range = format!("{v1}..main");

    let out = repo.tak(&["--commits", &range, "--limit", "1"]);
    assert!(out.status.success(), "{}", both(&out));
    assert_eq!(versions(&repo, &v4), ["v4"]);
    assert!(repo.notes(&v2).is_empty());
    assert!(stdout(&out).contains("2 more commit(s) beyond --limit 1"));

    let dry = repo.tak(&["--commits", &range, "--limit", "1", "--dry-run"]);
    let text = stdout(&dry);
    assert!(text.contains(&format!("{}  recorded", &v4[..12])), "{text}");
    assert!(
        text.contains(&format!("{}  would build", &broken[..12])),
        "{text}"
    );
    assert!(
        text.contains(&format!("{}  beyond --limit", &v2[..12])),
        "{text}"
    );
}

/// The current tak.toml may name a fixture an older tree does not have. That
/// commit records nothing — not even the benchmarks that could have run —
/// and says why.
#[test]
fn a_commit_missing_a_fixture_records_nothing() {
    let repo = Repo::new();
    config(
        &repo,
        r#"
[bench.fixture]
cmd = ["./tool"]
dir = "fixture"
runs = 2
warmup = 0
"#,
    );
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.git(&["add", ".gitignore"]);
    let base = repo.commit_tool(Some("v1"));
    let without = repo.commit_tool(Some("v2"));
    repo.write("fixture/input", "x\n");
    repo.git(&["add", "fixture"]);
    let with = repo.commit_tool(Some("v3"));

    let out = repo.tak(&["--commits", &format!("{base}..main")]);
    assert!(out.status.success(), "{}", both(&out));
    assert!(repo.notes(&without).is_empty(), "whole commit or nothing");
    assert!(
        stdout(&out).contains("`dir` fixture does not exist at this commit"),
        "{}",
        stdout(&out)
    );
    let mut benches: Vec<String> = repo.notes(&with).into_iter().map(|r| r.bench).collect();
    benches.sort();
    assert_eq!(benches, ["fixture", "startup"]);
}

#[test]
fn a_range_where_nothing_could_be_recorded_fails() {
    let repo = Repo::new();
    config(&repo, "");
    let [_, _, broken, _] = history(&repo);

    let out = repo.tak(&["--commits", &format!("{broken}~1..{broken}")]);
    assert!(!out.status.success(), "{}", both(&out));
    assert!(both(&out).contains("no commit in"), "{}", both(&out));
}

#[test]
fn a_build_table_is_required() {
    let repo = Repo::new();
    history(&repo);
    repo.write("tak.toml", "[bench.startup]\ncmd = [\"./tool\"]\n");
    let out = repo.tak(&["--commits", "main~1..main"]);
    assert!(!out.status.success());
    assert!(both(&out).contains("has no [build]"), "{}", both(&out));
    assert_eq!(repo.worktrees(), 1);
}

/// Each of these selects release binaries; with --commits they would be
/// silently ignored and a different series recorded from the one asked for.
#[test]
fn release_options_are_refused_with_commits() {
    let repo = Repo::new();
    config(&repo, "");
    history(&repo);
    for extra in [
        &["--bin", "tool"][..],
        &["--repo", "o/r"][..],
        &["--", "--help"][..],
    ] {
        let mut args = vec!["--commits", "main~1..main"];
        args.extend_from_slice(extra);
        let out = repo.tak(&args);
        assert!(!out.status.success(), "{extra:?} was accepted");
    }
    let out = repo.tak(&["--force"]);
    assert!(!out.status.success());
    assert!(both(&out).contains("--force only applies with --commits"));
}

/// Ctrl-C reaches tak and the build together, as a terminal delivers it to
/// the whole foreground process group. The checkout must not outlive tak.
#[test]
fn an_interrupted_build_leaves_no_checkout_behind() {
    use std::os::unix::process::CommandExt;

    let repo = Repo::new();
    history(&repo);
    repo.write(
        "tak.toml",
        "[build]\ncmd = [\"sleep\", \"30\"]\n[bench.startup]\ncmd = [\"./tool\"]\n",
    );
    let mut child = repo
        .tak_cmd(&["--commits", "main~1..main"])
        .process_group(0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    // Wait for the checkout to exist, so the signal lands during the build.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while repo.worktrees() < 2 {
        assert!(std::time::Instant::now() < deadline, "no checkout appeared");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Give tak a moment to start the build after the checkout.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let pgid = child.id() as i32;
    // SAFETY: signals a process group this test created.
    unsafe {
        libc::killpg(pgid, libc::SIGINT);
    }

    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if std::time::Instant::now() > deadline {
            // SAFETY: as above.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
            panic!("tak did not stop after SIGINT");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(130), "{status:?}");
    assert_eq!(repo.worktrees(), 1, "the checkout was removed");
    assert!(repo.tmp_is_empty(), "and the scratch directory");
}
