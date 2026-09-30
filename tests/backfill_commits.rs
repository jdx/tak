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

    // Nothing is built again, not even the commit whose build failed, and a
    // run with nothing new to record still succeeds: the range has its
    // history.
    let again = repo.tak(&["--commits", &range]);
    assert!(again.status.success(), "{}", both(&again));
    assert_eq!(repo.builds(), 3);
    assert!(
        stdout(&again).contains("2 already recorded"),
        "{}",
        stdout(&again)
    );
    assert_eq!(repo.notes(&v4).len(), 1);
    assert_eq!(repo.notes(&v2).len(), 1);

    let forced = repo.tak(&["--commits", &range, "--force"]);
    assert!(forced.status.success(), "{}", both(&forced));
    assert_eq!(repo.builds(), 6);
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

/// A subject added to a benchmark after some commits were recorded is still
/// missing on those commits, and is backfilled there without measuring the
/// subjects that already have a point.
#[test]
fn a_new_subject_is_backfilled_on_recorded_commits() {
    let repo = Repo::new();
    let [v1, v2, _, v4] = history(&repo);
    let subject = |name: &str| {
        format!("[bench.cmp.subject.{name}]\ncmd = [\"./tool\"]\nruns = 2\nwarmup = 0\n")
    };
    let toml = |subjects: &str| {
        format!(
            "[build]\ncmd = [\"sh\", \"-c\", \"echo built >> {} && cp tool.sh tool\"]\n{subjects}",
            repo.build_log().display()
        )
    };
    let range = format!("{v1}..main");
    repo.write("tak.toml", &toml(&subject("a")));
    let first = repo.tak(&["--commits", &range]);
    assert!(first.status.success(), "{}", both(&first));

    repo.write("tak.toml", &toml(&(subject("a") + &subject("b"))));
    let dry = repo.tak(&["--commits", &range, "--dry-run"]);
    assert!(
        stdout(&dry).contains(&format!("{}  would build (cmp (b))", &v4[..12])),
        "{}",
        stdout(&dry)
    );
    let second = repo.tak(&["--commits", &range]);
    assert!(second.status.success(), "{}", both(&second));
    for sha in [&v2, &v4] {
        let mut tools: Vec<String> = repo.notes(sha).into_iter().map(|r| r.tool).collect();
        tools.sort();
        assert_eq!(tools, ["a", "b"], "one point per subject on {sha}");
    }
}

/// A commit that cannot build is passed over by later runs, so `--limit`
/// keeps making progress past it. Changing `[build]` tries it again.
#[test]
fn a_failed_build_is_passed_over_next_time() {
    let repo = Repo::new();
    config(&repo, "");
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.git(&["add", ".gitignore"]);
    let v1 = repo.commit_tool(Some("v1"));
    let v2 = repo.commit_tool(Some("v2"));
    let broken = repo.commit_tool(None);
    let range = format!("{v1}..main");

    let first = repo.tak(&["--commits", &range, "--limit", "1"]);
    assert!(
        !first.status.success(),
        "nothing recorded: {}",
        both(&first)
    );
    assert_eq!(repo.builds(), 1);

    let dry = repo.tak(&["--commits", &range, "--dry-run"]);
    assert!(
        stdout(&dry).contains(&format!("{}  build failed before", &broken[..12])),
        "{}",
        stdout(&dry)
    );
    let second = repo.tak(&["--commits", &range, "--limit", "1"]);
    assert!(second.status.success(), "{}", both(&second));
    assert_eq!(versions(&repo, &v2), ["v2"], "the next commit was reached");
    assert!(stdout(&second).contains("1 commit(s) passed over"));
    assert_eq!(repo.builds(), 2);

    // A different runner class has its own memory: a toolchain change is
    // what a new class marks, and may be what makes the commit build.
    let other = repo
        .tak_cmd(&["--commits", &range, "--dry-run", "--runner", "other"])
        .output()
        .unwrap();
    assert!(
        stdout(&other).contains(&format!("{}  would build", &broken[..12])),
        "{}",
        stdout(&other)
    );

    // --force retries it, and so does a different [build].
    repo.tak(&["--commits", &range, "--force", "--limit", "1"]);
    assert_eq!(repo.builds(), 3);
    config(&repo, "[build.env]\nCHANGED = \"1\"\n");
    let changed = repo.tak(&["--commits", &range, "--dry-run"]);
    assert!(
        stdout(&changed).contains(&format!("{}  would build", &broken[..12])),
        "{}",
        stdout(&changed)
    );
}

/// The configured `dir` has no `..`, but an old commit can hold a symlink
/// where it expects a directory. Following one to the live tree would build
/// that tree instead of the commit.
#[test]
fn a_build_dir_symlinked_out_of_the_checkout_is_refused() {
    let repo = Repo::new();
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.git(&["add", ".gitignore"]);
    let base = repo.commit_tool(Some("v1"));
    let outside = repo.tmp.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.dir.join("app")).unwrap();
    repo.git(&["add", "app"]);
    let linked = repo.commit_tool(Some("v2"));
    repo.write(
        "tak.toml",
        &format!(
            "[build]\ncmd = [\"sh\", \"-c\", \"echo built >> {}\"]\ndir = \"app\"\n\
             [bench.startup]\ncmd = [\"./tool\"]\n",
            repo.build_log().display()
        ),
    );

    let out = repo.tak(&["--commits", &format!("{base}..main")]);
    assert!(
        stdout(&out).contains("app leads outside the checkout"),
        "{}",
        both(&out)
    );
    assert_eq!(repo.builds(), 0, "the build never ran");
    assert!(repo.notes(&linked).is_empty());
    assert_eq!(repo.worktrees(), 1);

    // The tree will hold the same symlink next time, so it is remembered
    // like a failed build rather than retried ahead of older commits.
    let dry = repo.tak(&["--commits", &format!("{base}..main"), "--dry-run"]);
    assert!(
        stdout(&dry).contains(&format!("{}  build failed before", &linked[..12])),
        "{}",
        stdout(&dry)
    );
}

/// Likewise a program: a committed symlink at the path the current tak.toml
/// runs would measure a binary from outside the commit.
#[test]
fn a_program_symlinked_out_of_the_checkout_is_not_measured() {
    let repo = Repo::new();
    repo.write(".gitignore", "tak.toml\n");
    repo.git(&["add", ".gitignore"]);
    let base = repo.commit_tool(Some("v1"));
    let outside = repo.tmp.parent().unwrap().join("outside-tool");
    std::fs::write(&outside, "#!/bin/sh\necho outside\n").unwrap();
    make_executable(&outside);
    std::os::unix::fs::symlink(&outside, repo.dir.join("tool")).unwrap();
    repo.git(&["add", "tool"]);
    let linked = repo.commit_tool(Some("v2"));
    repo.write(
        "tak.toml",
        "[build]\ncmd = [\"true\"]\n[bench.startup]\ncmd = [\"./tool\"]\nruns = 2\nwarmup = 0\n",
    );

    let out = repo.tak(&["--commits", &format!("{base}..main")]);
    assert!(
        stdout(&out).contains("tool leads outside the checkout"),
        "{}",
        both(&out)
    );
    assert!(repo.notes(&linked).is_empty());
}

/// A commit dated before 1970 is recorded with its own date, and does not
/// stop the rest of the range.
#[test]
fn a_commit_dated_before_1970_is_recorded() {
    let repo = Repo::new();
    config(&repo, "");
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.git(&["add", ".gitignore"]);
    let base = repo.commit_tool(Some("v1"));
    repo.write("tool.sh", "#!/bin/sh\necho v2\n");
    repo.git(&["add", "tool.sh"]);
    // `git commit` refuses a date before 1970 however GIT_COMMITTER_DATE
    // spells it, so the commit is written by hand, as an import from
    // another system could have.
    let tree = repo.git(&["write-tree"]);
    let who = "tak test <test@example.invalid> -14182940 +0000";
    let body = format!("tree {tree}\nparent {base}\nauthor {who}\ncommitter {who}\n\nv2\n");
    let object = repo.dir.join("commit-object");
    std::fs::write(&object, body).unwrap();
    let old = repo.git(&[
        "hash-object",
        "-t",
        "commit",
        "-w",
        "--literally",
        object.to_str().unwrap(),
    ]);
    std::fs::remove_file(&object).unwrap();
    repo.git(&["update-ref", "refs/heads/main", &old]);
    let after = repo.commit_tool(Some("v3"));

    let run = repo.tak(&["--commits", &format!("{base}..main")]);
    assert!(run.status.success(), "{}", both(&run));
    assert_eq!(versions(&repo, &after), ["v3"], "the range carried on");
    // `%ct` prints nothing for this commit; the raw object still has its
    // date, and the record carries it.
    let recs = repo.notes(&old);
    let rec = recs.first().unwrap_or_else(|| panic!("{}", both(&run)));
    assert_eq!(rec.version.as_deref(), Some("v2"));
    assert_eq!(rec.ts, "1969-07-20T20:17:40Z");
    assert!(!both(&run).contains("warning: no readable committer date"));
}

/// Start a backfill whose build records its pid and then sleeps, and wait
/// until that build is running. Returns tak and the build's pid.
fn start_slow_build(repo: &Repo) -> (std::process::Child, i32) {
    start_slow(repo, |pid| {
        format!(
            "[build]\ncmd = [\"sh\", \"-c\", \"{pid}\"]\n\
             [bench.startup]\ncmd = [\"./tool\"]\n"
        )
    })
}

/// The same, with the slow command in the measurement instead: the build is
/// quick, and the benchmark's one sample records its pid and sleeps.
fn start_slow_measurement(repo: &Repo) -> (std::process::Child, i32) {
    start_slow(repo, |pid| {
        format!(
            "[build]\ncmd = [\"true\"]\n\
             [bench.startup]\ncmd = [\"sh\", \"-c\", \"{pid}\"]\nruns = 1\nwarmup = 0\n"
        )
    })
}

/// Start a backfill of the newest commit with `toml` as its tak.toml, where
/// `toml` is given a shell snippet that records its pid and then sleeps, and
/// wait until that snippet is running.
fn start_slow(repo: &Repo, toml: impl Fn(&str) -> String) -> (std::process::Child, i32) {
    let (child, pid, _) = start_slow_piped(repo, toml, false);
    (child, pid)
}

/// [`start_slow`], optionally as the first command of a pipeline: tak's
/// stdout feeds a `cat` in tak's own process group, as a shell puts
/// `tak … | tee log` in one job. Returns the reader too.
fn start_slow_piped(
    repo: &Repo,
    toml: impl Fn(&str) -> String,
    piped: bool,
) -> (std::process::Child, i32, Option<std::process::Child>) {
    use std::os::unix::process::CommandExt;

    history(repo);
    let pidfile = repo.tmp.parent().unwrap().join("slow.pid");
    let snippet = format!(
        "echo $$ > {0}.tmp && mv {0}.tmp {0} && exec sleep 30",
        pidfile.display()
    );
    repo.write("tak.toml", &toml(&snippet));
    // Its own group, standing in for a terminal's foreground job.
    let mut child = repo
        .tak_cmd(&["--commits", "main~1..main"])
        .process_group(0)
        .stdout(if piped {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let reader = child.stdout.take().map(|out| {
        let log = std::fs::File::create(repo.tmp.parent().unwrap().join("tee.log")).unwrap();
        Command::new("cat")
            .process_group(child.id() as i32)
            .stdin(out)
            .stdout(log)
            .spawn()
            .unwrap()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(&pidfile) {
            break text.trim().parse().unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the build never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    (child, pid, reader)
}

/// A build or measurement tak killed on the way out is not the commit's
/// fault, and must not be remembered as a failure later runs pass over.
fn assert_no_failure_remembered(repo: &Repo) {
    let memory = repo.dir.join(".git/tak/backfill-build-failed");
    let text = std::fs::read_to_string(memory).unwrap_or_default();
    assert!(text.trim().is_empty(), "remembered: {text}");
}

/// Wait for `pid` to be gone, or for the deadline.
fn wait_gone(pid: i32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(pid, 0) } != 0 || is_zombie(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// Dead and waiting to be reaped. An orphan is reparented to PID 1, and in a
/// container whose PID 1 is not an init — `docker run … sleep infinity` —
/// nothing ever reaps it, so it exists for `kill(pid, 0)` forever.
fn is_zombie(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next().map(|st| st == "Z"))
        })
        .unwrap_or(false)
}

fn wait_for(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if std::time::Instant::now() > deadline {
            child.kill().ok();
            panic!("tak did not stop");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Ctrl-C, delivered as a terminal does to the whole foreground job. The
/// build runs in a group of its own, so it is tak that stops it; neither it
/// nor the checkout may outlive tak.
#[test]
fn an_interrupted_build_leaves_no_checkout_behind() {
    let repo = Repo::new();
    let (mut child, build) = start_slow_build(&repo);
    // SAFETY: signals the process group this test created.
    unsafe {
        libc::killpg(child.id() as i32, libc::SIGINT);
    }
    let status = wait_for(&mut child);
    assert_eq!(status.code(), Some(130), "{status:?}");
    assert!(wait_gone(build), "the build outlived tak");
    assert_no_failure_remembered(&repo);
    assert_eq!(repo.worktrees(), 1, "the checkout was removed");
    assert!(repo.tmp_is_empty(), "and the scratch directory");
}

/// SIGTERM to tak alone, as CI cancelling a job or `kill <pid>` sends it:
/// the build never receives it, so tak has to stop the build itself.
#[test]
fn a_sigterm_to_tak_alone_stops_the_build() {
    let repo = Repo::new();
    let (mut child, build) = start_slow_build(&repo);
    // SAFETY: signals the process this test spawned.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = wait_for(&mut child);
    assert_eq!(status.code(), Some(143), "{status:?}");
    assert!(wait_gone(build), "the build outlived tak");
    assert_no_failure_remembered(&repo);
    assert_eq!(repo.worktrees(), 1);
    assert!(repo.tmp_is_empty());
}

/// The same during the measurement: the measured command shares tak's own
/// process group, not the build's, and tak leads that group here as it does
/// as a terminal's job. Nothing is recorded, and nothing remembered.
#[test]
fn a_sigterm_during_measurement_stops_the_subject() {
    let repo = Repo::new();
    let (mut child, subject) = start_slow_measurement(&repo);
    let head = repo.git(&["rev-parse", "HEAD"]);
    // SAFETY: signals the process this test spawned.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = wait_for(&mut child);
    assert_eq!(status.code(), Some(143), "{status:?}");
    assert!(wait_gone(subject), "the measured command outlived tak");
    assert_no_failure_remembered(&repo);
    assert!(repo.notes(&head).is_empty());
    assert_eq!(repo.worktrees(), 1);
    assert!(repo.tmp_is_empty());
}

/// A commit that builds and then fails to measure is remembered like one
/// that fails to build, so `--limit 1` reaches the next commit on the next
/// run instead of rebuilding the same one forever. `--force`, or an edit to
/// tak.toml, tries it again.
#[test]
fn a_failed_measurement_is_passed_over_next_time() {
    let repo = Repo::new();
    config(&repo, "");
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.git(&["add", ".gitignore"]);
    let v1 = repo.commit_tool(Some("v1"));
    let v2 = repo.commit_tool(Some("v2"));
    repo.write("tool.sh", "#!/bin/sh\nexit 1\n");
    repo.git(&["commit", "-q", "-am", "broken benchmark"]);
    let broken = repo.git(&["rev-parse", "HEAD"]);
    let range = format!("{v1}..main");

    let first = repo.tak(&["--commits", &range, "--limit", "1"]);
    assert!(
        !first.status.success(),
        "nothing recorded: {}",
        both(&first)
    );
    assert!(
        stdout(&first).contains(&format!("measurement failed: {}", &broken[..12])),
        "{}",
        stdout(&first)
    );
    assert_eq!(repo.builds(), 1);

    let dry = repo.tak(&["--commits", &range, "--dry-run"]);
    assert!(
        stdout(&dry).contains(&format!("{}  measure failed before", &broken[..12])),
        "{}",
        stdout(&dry)
    );
    let second = repo.tak(&["--commits", &range, "--limit", "1"]);
    assert!(second.status.success(), "{}", both(&second));
    assert_eq!(versions(&repo, &v2), ["v2"], "the next commit was reached");
    assert!(
        stdout(&second).contains(
            "1 commit(s) passed over: they failed in an earlier run (0 build, 1 measurement"
        ),
        "{}",
        stdout(&second)
    );
    assert_eq!(repo.builds(), 2);

    let forced = repo.tak(&["--commits", &range, "--force", "--limit", "1"]);
    assert!(
        stdout(&forced).contains(&broken[..12]),
        "{}",
        stdout(&forced)
    );
    assert_eq!(repo.builds(), 3, "--force built it again");

    config(&repo, "# edited\n");
    let edited = repo.tak(&["--commits", &range, "--dry-run"]);
    assert!(
        stdout(&edited).contains(&format!("{}  would build", &broken[..12])),
        "{}",
        stdout(&edited)
    );
}

/// A `setup` is the old tree's own code, run after the paths were first
/// checked; one that leaves a symlink out of the checkout where `dir` is must
/// stop the subject before it runs there.
#[test]
fn a_setup_that_links_out_of_the_checkout_is_refused() {
    let repo = Repo::new();
    history(&repo);
    let outside = repo.tmp.parent().unwrap().join("outside-dir");
    std::fs::create_dir_all(&outside).unwrap();
    repo.write(
        "tak.toml",
        &format!(
            "[build]\ncmd = [\"true\"]\n\
             [bench.fixture]\ncmd = [\"sh\", \"-c\", \"touch ran\"]\ndir = \"work\"\n\
             setup = [\"ln\", \"-s\", \"{}\", \"work\"]\nruns = 1\nwarmup = 0\n",
            outside.display()
        ),
    );
    let head = repo.git(&["rev-parse", "HEAD"]);
    let out = repo.tak(&["--commits", "main~1..main"]);
    assert!(
        stdout(&out).contains("work leads outside the checkout"),
        "{}",
        both(&out)
    );
    assert!(!outside.join("ran").exists(), "the subject ran outside");
    assert!(repo.notes(&head).is_empty());
}

/// Counting was asked for, valgrind is there, and the count failed: a
/// timing-only record would look like counters were off, so nothing is
/// recorded for the commit and the failure is remembered like any other.
#[test]
fn a_failed_instruction_count_records_nothing() {
    if !tak_cli::measure::valgrind_available() {
        eprintln!("skipping: valgrind not installed");
        return;
    }
    let repo = Repo::new();
    history(&repo);
    // Exits 3 under valgrind and 0 otherwise: valgrind's tool is mapped into
    // the shell, and the `grep` it spawns runs natively and sees it.
    repo.write(
        "tak.toml",
        "[build]\ncmd = [\"true\"]\n\
         [bench.startup]\n\
         cmd = [\"sh\", \"-c\", \"grep -q -e cachegrind -e valgrind /proc/$$/maps && exit 3; exit 0\"]\n\
         runs = 2\nwarmup = 0\n",
    );
    let head = repo.git(&["rev-parse", "HEAD"]);
    let out = repo.tak(&["--commits", "main~1..main"]);
    assert!(
        stdout(&out).contains("instruction counting failed: startup"),
        "{}",
        both(&out)
    );
    assert!(repo.notes(&head).is_empty());
    let dry = repo.tak(&["--commits", "main~1..main", "--dry-run"]);
    assert!(
        stdout(&dry).contains("measure failed before"),
        "{}",
        stdout(&dry)
    );
}

/// SIGTERM to tak alone while it is the first command of a pipeline, as in
/// `tak backfill … | tee log`: the shell puts both in one process group, and
/// stopping the measured command must not stop the reader, which finishes on
/// its own once tak's output closes.
#[test]
fn a_sigterm_spares_the_rest_of_a_pipeline() {
    let repo = Repo::new();
    let (mut child, subject, reader) = start_slow_piped(
        &repo,
        |pid| {
            format!(
                "[build]\ncmd = [\"true\"]\n\
                 [bench.startup]\ncmd = [\"sh\", \"-c\", \"{pid}\"]\nruns = 1\nwarmup = 0\n"
            )
        },
        true,
    );
    let mut reader = reader.expect("a reader");
    // SAFETY: signals the process this test spawned.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = wait_for(&mut child);
    assert_eq!(status.code(), Some(143), "{status:?}");
    assert!(wait_gone(subject), "the measured command outlived tak");
    let read = wait_for(&mut reader);
    assert!(read.success(), "the reader was stopped too: {read:?}");
    assert_eq!(repo.worktrees(), 1);
}

/// A measurement failure is remembered for its benchmark only: a later run
/// of another benchmark still measures that commit.
#[test]
fn a_failed_benchmark_does_not_pass_over_the_others() {
    let repo = Repo::new();
    history(&repo);
    repo.write(
        "tak.toml",
        "[build]\ncmd = [\"true\"]\n\
         [bench.broken]\ncmd = [\"sh\", \"-c\", \"exit 1\"]\nruns = 1\nwarmup = 0\n\
         [bench.fine]\ncmd = [\"sh\", \"-c\", \"exit 0\"]\nruns = 1\nwarmup = 0\n",
    );
    let head = repo.git(&["rev-parse", "HEAD"]);
    let range = "main~1..main";

    let first = repo.tak(&["--commits", range, "--bench", "broken"]);
    assert!(!first.status.success(), "{}", both(&first));
    let second = repo.tak(&["--commits", range, "--bench", "fine"]);
    assert!(second.status.success(), "{}", both(&second));
    let benches: Vec<String> = repo.notes(&head).into_iter().map(|r| r.bench).collect();
    assert_eq!(benches, ["fine"]);

    // Without --bench: `fine` is recorded and `broken` failed before, so
    // there is nothing to build.
    let dry = repo.tak(&["--commits", range, "--dry-run"]);
    assert!(
        stdout(&dry).contains(&format!("{}  measure failed before", &head[..12])),
        "{}",
        stdout(&dry)
    );
}

/// Every setup of a multi-subject benchmark runs before any sampling, so a
/// later subject's setup can swap an earlier one's already-checked `dir` for
/// a symlink. The check runs again once all of them have.
#[test]
fn a_later_setup_cannot_move_an_earlier_subjects_dir() {
    let repo = Repo::new();
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.write("wx/keep", "");
    repo.git(&["add", ".gitignore", "wx"]);
    repo.commit_tool(Some("v1"));
    repo.commit_tool(Some("v2"));
    let outside = repo.tmp.parent().unwrap().join("outside-dir");
    std::fs::create_dir_all(&outside).unwrap();
    repo.write(
        "tak.toml",
        &format!(
            "[build]\ncmd = [\"true\"]\n\
             [bench.cmp]\nruns = 1\nwarmup = 0\n\
             [bench.cmp.subject.x]\ncmd = [\"sh\", \"-c\", \"touch ran\"]\ndir = \"wx\"\n\
             [bench.cmp.subject.y]\ncmd = [\"true\"]\n\
             setup = [\"sh\", \"-c\", \"rm -rf wx && ln -s {} wx\"]\n",
            outside.display()
        ),
    );
    let out = repo.tak(&["--commits", "HEAD~1..HEAD"]);
    assert!(
        both(&out).contains("wx leads outside the checkout"),
        "{}",
        both(&out)
    );
    assert!(!outside.join("ran").exists(), "x ran outside the checkout");
}

/// `prepare` runs before every sample and can do the same, so the check
/// also runs after it, before the sample is timed.
#[test]
fn a_prepare_cannot_move_its_dir_out_of_the_checkout() {
    let repo = Repo::new();
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.write("wx/keep", "");
    repo.git(&["add", ".gitignore", "wx"]);
    repo.commit_tool(Some("v1"));
    repo.commit_tool(Some("v2"));
    let outside = repo.tmp.parent().unwrap().join("outside-dir");
    std::fs::create_dir_all(&outside).unwrap();
    repo.write(
        "tak.toml",
        &format!(
            "[build]\ncmd = [\"true\"]\n\
             [bench.fixture]\ncmd = [\"sh\", \"-c\", \"touch ran\"]\ndir = \"wx\"\n\
             prepare = [\"sh\", \"-c\", \"d=$PWD && cd / && rm -rf \\\"$d\\\" && ln -s {} \\\"$d\\\"\"]\n\
             runs = 1\nwarmup = 0\n",
            outside.display()
        ),
    );
    let out = repo.tak(&["--commits", "HEAD~1..HEAD"]);
    assert!(
        both(&out).contains("wx leads outside the checkout"),
        "{}",
        both(&out)
    );
    assert!(
        !outside.join("ran").exists(),
        "the sample ran outside the checkout"
    );
}

/// A build failure retried with `--force` that then builds, and fails to
/// measure one benchmark, no longer counts as a build failure: the next plain
/// run still measures the commit's other benchmarks.
#[test]
fn a_forced_retry_that_builds_clears_the_build_failure() {
    let repo = Repo::new();
    history(&repo);
    let flag = repo.tmp.parent().unwrap().join("builds-now");
    repo.write(
        "tak.toml",
        &format!(
            "[build]\ncmd = [\"test\", \"-e\", \"{}\"]\n\
             [bench.a]\ncmd = [\"sh\", \"-c\", \"exit 1\"]\nruns = 1\nwarmup = 0\n\
             [bench.b]\ncmd = [\"sh\", \"-c\", \"exit 0\"]\nruns = 1\nwarmup = 0\n",
            flag.display()
        ),
    );
    let head = repo.git(&["rev-parse", "HEAD"]);
    let range = "main~1..main";

    let first = repo.tak(&["--commits", range]);
    assert!(
        stdout(&first).contains("build failed"),
        "{}",
        stdout(&first)
    );
    std::fs::write(&flag, "").unwrap();
    let forced = repo.tak(&["--commits", range, "--force", "--bench", "a"]);
    assert!(
        stdout(&forced).contains("measurement failed"),
        "{}",
        both(&forced)
    );
    let plain = repo.tak(&["--commits", range]);
    assert!(plain.status.success(), "{}", both(&plain));
    let benches: Vec<String> = repo.notes(&head).into_iter().map(|r| r.bench).collect();
    assert_eq!(benches, ["b"]);
}

/// A command can leave something running in its process group — a server a
/// `setup` starts on purpose. tak leaves it alone while the backfill runs,
/// and stops it on interrupt with the command running then, since the
/// checkout it runs in is about to be deleted.
#[test]
fn an_interrupt_stops_what_an_earlier_command_left_running() {
    let repo = Repo::new();
    let helper = repo.tmp.parent().unwrap().join("helper.pid");
    let (mut child, subject, _) = start_slow_piped(
        &repo,
        |pid| {
            format!(
                "[build]\ncmd = [\"true\"]\n\
                 [bench.startup]\ncmd = [\"sh\", \"-c\", \"{pid}\"]\nruns = 1\nwarmup = 0\n\
                 setup = [\"sh\", \"-c\", \"sleep 30 & echo $! > {}\"]\n",
                helper.display()
            )
        },
        false,
    );
    let helper: i32 = std::fs::read_to_string(&helper)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only checks that the process exists.
    assert_eq!(unsafe { libc::kill(helper, 0) }, 0, "the helper is running");
    // SAFETY: signals the process this test spawned.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = wait_for(&mut child);
    assert_eq!(status.code(), Some(143), "{status:?}");
    assert!(wait_gone(subject), "the measured command outlived tak");
    assert!(wait_gone(helper), "what setup left running outlived tak");
    assert_eq!(repo.worktrees(), 1);
}

/// `prepare`, `check`, `version_cmd` and `setup` run from the checkout as
/// much as `cmd` does, so a committed symlink at one of their paths is
/// refused the same way.
#[test]
fn every_program_from_the_checkout_is_checked() {
    let outside = |repo: &Repo| {
        let p = repo.tmp.parent().unwrap().join("outside-tool");
        std::fs::write(&p, "#!/bin/sh\ntouch \"$0.ran\"\n").unwrap();
        make_executable(&p);
        p
    };
    for key in ["prepare", "check", "version_cmd", "setup"] {
        let repo = Repo::new();
        repo.write(".gitignore", "tak.toml\n");
        repo.git(&["add", ".gitignore"]);
        let base = repo.commit_tool(Some("v1"));
        let target = outside(&repo);
        std::os::unix::fs::symlink(&target, repo.dir.join("helper")).unwrap();
        repo.git(&["add", "helper"]);
        repo.commit_tool(Some("v2"));
        repo.write(
            "tak.toml",
            &format!(
                "[build]\ncmd = [\"true\"]\n\
                 [bench.startup]\ncmd = [\"sh\", \"-c\", \"exit 0\"]\n{key} = [\"./helper\"]\n\
                 runs = 1\nwarmup = 0\n"
            ),
        );
        let out = repo.tak(&["--commits", &format!("{base}..main")]);
        assert!(
            both(&out).contains("helper leads outside the checkout"),
            "{key}: {}",
            both(&out)
        );
        assert!(
            !target.with_extension("ran").exists()
                && !repo.tmp.parent().unwrap().join("outside-tool.ran").exists(),
            "{key}: the program outside the checkout ran"
        );
    }
}

/// The build's own program, when it is one of the tree's files, likewise.
#[test]
fn a_build_program_symlinked_out_of_the_checkout_is_refused() {
    let repo = Repo::new();
    repo.write(".gitignore", "tak.toml\n");
    repo.git(&["add", ".gitignore"]);
    let base = repo.commit_tool(Some("v1"));
    let target = repo.tmp.parent().unwrap().join("outside-build");
    std::fs::write(&target, "#!/bin/sh\ntouch \"$0.ran\"\n").unwrap();
    make_executable(&target);
    std::os::unix::fs::symlink(&target, repo.dir.join("build.sh")).unwrap();
    repo.git(&["add", "build.sh"]);
    let linked = repo.commit_tool(Some("v2"));
    repo.write(
        "tak.toml",
        "[build]\ncmd = [\"./build.sh\"]\n[bench.startup]\ncmd = [\"./tool.sh\"]\n",
    );
    let out = repo.tak(&["--commits", &format!("{base}..main")]);
    assert!(
        stdout(&out).contains("build.sh leads outside the checkout"),
        "{}",
        both(&out)
    );
    assert!(
        !repo
            .tmp
            .parent()
            .unwrap()
            .join("outside-build.ran")
            .exists()
    );
    assert!(repo.notes(&linked).is_empty());
}

/// A two-commit history whose newest tree has a `wx` directory, and a
/// directory outside the repository for a symlink to point at.
fn history_with_wx(repo: &Repo) -> PathBuf {
    repo.write(".gitignore", "tool\ntak.toml\n");
    repo.write("wx/keep", "");
    repo.write("wy/keep", "");
    repo.git(&["add", ".gitignore", "wx", "wy"]);
    repo.commit_tool(Some("v1"));
    repo.commit_tool(Some("v2"));
    let outside = repo.tmp.parent().unwrap().join("outside-dir");
    std::fs::create_dir_all(&outside).unwrap();
    outside
}

/// A sample can swap its own `dir` for a symlink, and the next sample's
/// `prepare` runs there before the sample does. The check runs before that
/// prepare too, not only after it.
#[test]
fn a_prepare_does_not_run_through_a_dir_the_last_sample_moved() {
    let repo = Repo::new();
    let outside = history_with_wx(&repo);
    repo.write(
        "tak.toml",
        &format!(
            "[build]\ncmd = [\"true\"]\n\
             [bench.fixture]\n\
             cmd = [\"sh\", \"-c\", \"d=$PWD && cd / && rm -rf \\\"$d\\\" && ln -s {} \\\"$d\\\"\"]\n\
             dir = \"wx\"\nprepare = [\"sh\", \"-c\", \"touch prepared\"]\nruns = 2\nwarmup = 0\n",
            outside.display()
        ),
    );
    let out = repo.tak(&["--commits", "HEAD~1..HEAD"]);
    assert!(
        both(&out).contains("wx leads outside the checkout"),
        "{}",
        both(&out)
    );
    assert!(
        !outside.join("prepared").exists(),
        "the second prepare ran outside the checkout"
    );
}

/// Every subject's setup runs before any sample, so an earlier setup can
/// move a path a later one acts through. Each setup is checked before it
/// runs, not only once all of them have.
#[test]
fn a_setup_does_not_run_through_a_dir_an_earlier_setup_moved() {
    let repo = Repo::new();
    let outside = history_with_wx(&repo);
    repo.write(
        "tak.toml",
        &format!(
            "[build]\ncmd = [\"true\"]\n\
             [bench.cmp]\nruns = 1\nwarmup = 0\n\
             [bench.cmp.subject.x]\ncmd = [\"true\"]\n\
             setup = [\"sh\", \"-c\", \"rm -rf wy && ln -s {} wy\"]\n\
             [bench.cmp.subject.y]\ncmd = [\"true\"]\ndir = \"wy\"\n\
             setup = [\"sh\", \"-c\", \"touch wy/set-up\"]\n",
            outside.display()
        ),
    );
    let out = repo.tak(&["--commits", "HEAD~1..HEAD"]);
    assert!(
        both(&out).contains("wy leads outside the checkout"),
        "{}",
        both(&out)
    );
    assert!(
        !outside.join("set-up").exists(),
        "y's setup ran through the moved directory"
    );
}
