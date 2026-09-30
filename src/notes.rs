//! Storage: benchmark results as git notes under `refs/notes/tak`.
//!
//! Why notes rather than an orphan branch or a committed file:
//!
//! - The data is *about a commit*, which is exactly what notes are for.
//! - `cat_sort_uniq` resolves concurrent writers with no custom merge driver.
//! - The notes tree is keyed by commit SHA as *path names* and does not reference
//!   the annotated commits, so a single shallow fetch of this one ref returns the
//!   entire history without cloning the repository — measured at 36ms / 124K for
//!   100 commits × 6 benchmarks, with zero project commit objects transferred.
//!   That property is what makes a hosted dashboard cheap.
//!
//! All network operations shell out to `git` on purpose. `actions/checkout` sets
//! up auth via `http.extraheader`, and users have credential helpers, SSH agents
//! and corporate proxies; reimplementing any of that is a trap. Local object
//! access can move to `gix` later without changing this boundary.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

use crate::record::{Record, parse_note};

pub const NOTES_REF: &str = "refs/notes/tak";

/// Scratch ref the remote is fetched into, so a fetch never lands directly on
/// the ref holding records that have not been pushed yet.
const REMOTE_REF: &str = "refs/notes/tak-remote";

/// Refspec for reading. Forced, which is safe because it only ever overwrites
/// the scratch ref.
const FETCH_REFSPEC: &str = "+refs/notes/tak:refs/notes/tak-remote";

/// Refspec for writing, deliberately **not** forced.
///
/// A forced push always succeeds. That makes the retry-and-merge loop below
/// unreachable and lets any writer with a stale or empty local ref replace the
/// entire remote history in one shot. It is not a hypothetical: a CI job whose
/// checkout had never fetched notes recorded one measurement, pushed, and
/// destroyed 60 backfilled ones in jdx/aube.
///
/// Without the `+`, that push is rejected as a non-fast-forward, and the loop
/// fetches the remote, merges it in with cat_sort_uniq, and tries again.
const PUSH_REFSPEC: &str = "refs/notes/tak:refs/notes/tak";

/// Refspec suggested to users for plain `git fetch`, which has no local records
/// to lose because it is a read-only convenience for people who never run tak.
const USER_FETCH_REFSPEC: &str = "+refs/notes/tak:refs/notes/tak";
/// Number of fetch/merge/push attempts before giving up on a contended push.
const PUSH_ATTEMPTS: u32 = 5;

fn git(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Fallback identity arguments for commands that create a commit.
///
/// `git notes add` and `git notes merge` write commits, so git demands an
/// author. Containers and fresh CI images routinely have none configured, and
/// aborting a whole backfill for want of a name nobody will ever read is not a
/// useful failure. Only applied when the user has not set one, so a real
/// identity is never overridden.
fn identity_args() -> Vec<&'static str> {
    let configured = |key: &str| {
        Command::new("git")
            .args(["config", "--get", key])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    // Checked independently: a machine with `user.name` set but no email would
    // otherwise have its real name replaced by the placeholder.
    let mut args = Vec::new();
    if !configured("user.name") {
        args.extend_from_slice(&["-c", "user.name=tak"]);
    }
    if !configured("user.email") {
        args.extend_from_slice(&["-c", "user.email=tak@localhost"]);
    }
    args
}

/// Like [`git`] but prepends a fallback identity, for commands that commit.
fn git_committing(args: &[&str]) -> Result<String> {
    let mut full = identity_args();
    full.extend_from_slice(args);
    git(&full)
}

/// Like [`git`] but returns the failure instead of raising, for calls whose
/// failure is an expected outcome (a rejected push, a missing note).
fn git_ok(args: &[&str]) -> Result<(bool, String)> {
    let out = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
    let text = if out.status.success() {
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    } else {
        String::from_utf8_lossy(&out.stderr).trim_end().to_string()
    };
    Ok((out.status.success(), text))
}

/// Pull the notes ref from `remote`.
///
/// Never fatal: a developer may be offline, or the remote may have no notes yet.
/// Callers fall back to whatever is in the local ref.
pub fn fetch(remote: &str) -> Result<bool> {
    let (ok, _) = git_ok(&["fetch", "--quiet", "--depth", "1", remote, FETCH_REFSPEC])?;
    if !ok {
        return Ok(false);
    }
    absorb_remote()?;
    Ok(true)
}

/// Fold the fetched remote notes into the local ref, keeping both sides.
///
/// Reading used to fetch straight onto `refs/notes/tak`, so `tak history` after
/// a local `tak run --record` threw the local record away before it could be
/// pushed. Merging instead of overwriting is the same choice the push path
/// makes, for the same reason: neither side of this ref is authoritative.
fn absorb_remote() -> Result<()> {
    // Nothing local yet: adopt the remote wholesale. `notes merge` needs a ref
    // to merge into and would fail here.
    let (has_local, _) = git_ok(&["rev-parse", "--verify", "--quiet", NOTES_REF])?;
    if !has_local {
        git(&["update-ref", NOTES_REF, REMOTE_REF])?;
        return Ok(());
    }
    git_committing(&[
        "notes",
        "--ref",
        NOTES_REF,
        "merge",
        "-s",
        "cat_sort_uniq",
        REMOTE_REF,
    ])?;
    Ok(())
}

/// Read every record attached to `commit`, after refreshing from the remote.
///
/// The fetch is deliberately inside the read path. Users must never have to know
/// that notes exist, let alone that they need a refspec — that seam is the single
/// biggest usability risk in this design.
pub fn read(remote: Option<&str>, commit: &str) -> Result<Vec<Record>> {
    if let Some(r) = remote {
        let _ = fetch(r);
    }
    let (ok, body) = git_ok(&["notes", "--ref", NOTES_REF, "show", commit])?;
    if !ok {
        // No note for this commit is a normal state, not an error.
        return Ok(vec![]);
    }
    Ok(parse_note(&body))
}

/// Append records to `commit`'s note locally, preserving anything already there.
pub fn append(commit: &str, records: &[Record]) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let mut lines: Vec<String> = Vec::new();
    let (ok, existing) = git_ok(&["notes", "--ref", NOTES_REF, "show", commit])?;
    if ok {
        lines.extend(existing.lines().map(str::to_string));
    }
    for r in records {
        lines.push(r.to_line()?);
    }
    // Sort and dedupe locally so the note matches what cat_sort_uniq would
    // produce on merge; otherwise a local write and a remote merge of the same
    // data yield different bytes.
    lines.sort();
    lines.dedup();

    git_committing(&[
        "notes",
        "--ref",
        NOTES_REF,
        "add",
        "-f",
        "-m",
        &lines.join("\n"),
        commit,
    ])?;
    Ok(())
}

/// Push the notes ref, resolving races against other CI jobs.
///
/// Two runners finishing at once will both try to push; the loser is rejected,
/// re-fetches, merges with `cat_sort_uniq` and retries. Verified end-to-end: both
/// runners' measurements survive the merge.
pub fn push(remote: &str) -> Result<()> {
    for attempt in 1..=PUSH_ATTEMPTS {
        let (ok, err) = git_ok(&["push", "--quiet", remote, PUSH_REFSPEC])?;
        if ok {
            return Ok(());
        }
        if attempt == PUSH_ATTEMPTS {
            bail!("could not push {NOTES_REF} after {PUSH_ATTEMPTS} attempts: {err}");
        }
        // Fetch the winner's ref to a scratch location, then merge into ours.
        git(&["fetch", "--quiet", remote, FETCH_REFSPEC])?;
        absorb_remote()?;
    }
    unreachable!()
}

/// Resolve a revision to the full SHA of the commit it names.
///
/// `^{commit}` matters: `git rev-parse v1.2.3` on an *annotated* tag returns the
/// tag object, not the commit, and notes are attached to commits. Without the
/// peel, `tak history v1.2.3` and `tak compare v1.2.3` silently find nothing on
/// exactly the revisions people are most likely to name. Harmless for branches
/// and raw SHAs, which peel to themselves.
pub fn rev_parse(rev: &str) -> Result<String> {
    git(&["rev-parse", &format!("{rev}^{{commit}}")])
}

/// The most recent `n` commits reachable from `rev`, newest first.
///
/// First-parent only. A merge commit's second parent is the branch that was
/// merged, and walking into it interleaves a feature branch's measurements with
/// the trunk's — which makes a trend line jump around for reasons that have
/// nothing to do with the trunk.
pub fn rev_list(rev: &str, n: usize) -> Result<Vec<String>> {
    let n = n.to_string();
    let out = git(&["rev-list", "--first-parent", "-n", &n, rev])?;
    Ok(out.lines().map(str::to_string).collect())
}

/// The values of trailer `key` on every commit in `base..head`, one line per
/// commit: its SHA, a NUL, then the values joined by commas.
///
/// Every commit in the range, not first-parent only — the opposite choice to
/// [`rev_list`], for a different question. A trend follows the trunk; this asks
/// what the change being measured declares, and under a merge-commit workflow
/// the declaration sits on the branch commits, reachable only through the
/// merge's second parent. Under squash-merge the range is the one squashed
/// commit, and whatever trailers survived into its message.
///
/// `unfold` joins a value git wrapped onto continuation lines, and the comma
/// separator keeps repeated trailers on one line, so a line is always exactly
/// one commit. Commits with no such trailer still print their SHA; the parser
/// skips the empty value.
///
/// `--no-show-signature` for the same reason [`log`] passes it: a user's
/// `log.showSignature` would otherwise put GPG output on stdout, between the
/// lines this parses. `--end-of-options` and `--` keep the range a revision
/// whatever it looks like.
///
/// `first_parent` narrows the range to the trunk's own commits, for `tak
/// detect`: after a merge, what landed on main is the commit on its
/// first-parent line — the squash, the rebased commit, or the merge itself —
/// and that is the history a main-branch check has reviewed as merged. A
/// branch commit behind a merge's second parent was only ever gated on its
/// pull request.
pub fn trailers(base: &str, head: &str, key: &str, first_parent: bool) -> Result<String> {
    let format = format!("--format=%H%x00%(trailers:key={key},valueonly,unfold,separator=%x2C)");
    let range = format!("{base}..{head}");
    let mut args = vec!["log", "--no-show-signature"];
    if first_parent {
        args.push("--first-parent");
    }
    args.extend([format.as_str(), "--end-of-options", &range, "--"]);
    git(&args)
}

/// One commit on a first-parent walk, with whatever tak recorded on it.
#[derive(Debug, Clone)]
pub struct Logged {
    pub sha: String,
    /// Committer date, strict ISO 8601. The committer date rather than the
    /// author date because a trunk timeline is about when a change landed; a
    /// branch rebased and merged a month later was authored long before it
    /// could have moved anything.
    pub date: String,
    pub subject: String,
    /// Empty for a commit nothing was recorded on — the common case.
    pub records: Vec<Record>,
}

/// Fields per commit in [`log`]'s output: SHA, date, subject, note.
const LOG_FIELDS: usize = 4;

/// Every commit on `rev`'s first-parent history, newest first, with its records.
///
/// One `git log`, with each note inlined through `%N`, rather than a
/// `notes show` per commit: a series view wants hundreds of commits, and a
/// subprocess each put a noticeable pause in front of what should be an
/// instant read of local objects.
///
/// Every field ends in NUL (`%x00` between fields, `-z` after the last), the
/// one byte none of them can hold: git will not store it in a commit message,
/// and JSON escapes it. A printable-looking separator such as `\x1e` was
/// tried first and can appear in a subject, which cut that commit in two and
/// dropped its measurements.
///
/// First-parent for the same reason as [`rev_list`]. `--no-notes` first clears
/// whatever refs `core.notesRef` and `notes.displayRef` configure, so only this
/// ref's notes reach `%N` and are parsed as records — explicitly, rather than
/// relying on an explicit `--notes=<ref>` happening to replace the defaults.
///
/// `max` bounds the walk to the newest `max` commits, for a caller that has to
/// stop somewhere on a trunk of hundreds of thousands of commits and needs to
/// know whether it did.
pub fn log(rev: &str, max: Option<usize>) -> Result<Vec<Logged>> {
    walk(rev, max, |_| false)
}

/// [`log`], stopping as soon as `done` says the commit just read is the last
/// one needed. That commit is kept.
///
/// For a limit that counts something `--max-count` cannot: `tak log -n 20`
/// wants twenty *recorded* commits, and how far back they reach is only known
/// by reading. Collecting the whole walk first read every note on the trunk,
/// and held all of them at once, to print twenty.
pub fn log_until(rev: &str, done: impl FnMut(&Logged) -> bool) -> Result<Vec<Logged>> {
    walk(rev, None, done)
}

fn walk(rev: &str, max: Option<usize>, done: impl FnMut(&Logged) -> bool) -> Result<Vec<Logged>> {
    let notes = format!("--notes={NOTES_REF}");
    let limit = max.map(|n| format!("--max-count={n}"));
    let mut args = vec![
        "log",
        "-z",
        "--first-parent",
        "--no-show-signature",
        "--no-notes",
        &notes,
        "--format=%H%x00%cI%x00%s%x00%N",
    ];
    if let Some(limit) = &limit {
        args.push(limit);
    }
    args.extend(["--end-of-options", rev, "--"]);
    let what = || format!("`git {}`", args.join(" "));

    let mut child = Command::new("git")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {}", what()))?;
    // Drained on a thread of its own, so git blocked writing a full stderr
    // pipe can never be waiting on us blocked reading its stdout.
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    // `read_log` owns the pipe and drops it on return, so when it stops early
    // git's next write fails before the kill below lands.
    let read = read_log(
        BufReader::new(child.stdout.take().expect("stdout is piped")),
        done,
    );

    let stopped = matches!(read, Ok((_, true)));
    if stopped || read.is_err() {
        // Stopping early is the point, so how git ends afterwards — killed,
        // or dead of a broken pipe mid-write — is not a failure to report.
        let _ = child.kill();
    }
    // Reaped on every path, so no zombie outlives the walk.
    let status = child
        .wait()
        .with_context(|| format!("failed to wait for {}", what()))?;
    let (logged, _) = read.with_context(|| format!("failed to read {}", what()))?;
    // Git's own stderr closes when it dies, killed or not, but anything it
    // spawned can inherit the pipe and hold it open. Only a failure needs
    // the text, so only a failure waits for it — as `Command::output` would
    // — and an early stop leaves the thread to finish on its own.
    if !stopped && !status.success() {
        let errors = errors.join().unwrap_or_default();
        bail!("{} failed: {}", what(), errors.trim());
    }
    Ok(logged)
}

/// Parse [`log`]'s output a commit at a time, until `done` accepts one or the
/// output ends. The flag beside the commits says which.
fn read_log(
    mut out: impl BufRead,
    mut done: impl FnMut(&Logged) -> bool,
) -> std::io::Result<(Vec<Logged>, bool)> {
    let mut logged = Vec::new();
    let mut fields: Vec<String> = Vec::with_capacity(LOG_FIELDS);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if out.read_until(0, &mut buf)? == 0 {
            // Whatever is left in `fields` is the empty piece after the final
            // terminator, or a group git never finished; neither is a commit.
            return Ok((logged, false));
        }
        if buf.last() == Some(&0) {
            buf.pop();
        }
        fields.push(String::from_utf8_lossy(&buf).into_owned());
        if fields.len() < LOG_FIELDS {
            continue;
        }
        let entry = parse_entry(&fields);
        fields.clear();
        if let Some(c) = entry {
            let last = done(&c);
            logged.push(c);
            if last {
                return Ok((logged, true));
            }
        }
    }
}

fn parse_entry(fields: &[String]) -> Option<Logged> {
    let [sha, date, subject, note] = fields else {
        return None;
    };
    // Nothing else here can come out of git misaligned, but a note is
    // arbitrary bytes a person could have written by hand; refusing a group
    // that does not start with a SHA stops one such note from inventing a
    // commit.
    if sha.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(Logged {
        sha: sha.clone(),
        date: date.clone(),
        subject: subject.clone(),
        records: parse_note(note),
    })
}

/// Whether `sha` is where this clone's history was cut off, so a walk that
/// ended there ran out of *clone* rather than out of project.
///
/// Asked of the commit the walk ended on rather than of the repository as a
/// whole. `rev-parse --is-shallow-repository` answers for every ref, and the
/// notes refresh is itself a `--depth 1` fetch: after the first one, a full
/// clone reports itself shallow because the *notes* history is, and a report
/// built in it warned that older measurements might be missing when none were.
///
/// `actions/checkout` defaults to a depth of one, so a genuinely cut-off walk
/// is the normal state of a CI checkout rather than an edge case.
pub fn is_shallow_boundary(sha: &str) -> bool {
    let Ok(path) = git(&["rev-parse", "--git-path", "shallow"]) else {
        return false;
    };
    // No file is the common case: nothing in this clone is shallow.
    std::fs::read_to_string(path).is_ok_and(|grafts| grafts.lines().any(|l| l.trim() == sha))
}

/// Teach plain `git fetch` about the notes ref, so the data is visible to users
/// who never run `tak`. A convenience, not load-bearing — every `tak` read path
/// fetches for itself.
pub fn install_refspec(remote: &str) -> Result<()> {
    let key = format!("remote.{remote}.fetch");
    let (_, existing) = git_ok(&["config", "--get-all", &key])?;
    if existing.lines().any(|l| l.trim() == USER_FETCH_REFSPEC) {
        return Ok(());
    }
    git(&["config", "--add", &key, USER_FETCH_REFSPEC])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str =
        r#"{"bench":"a","metrics":{"instructions":5.0},"runner":"r","tool":"self","ts":"t","v":1}"#;

    /// `%N` ends with a newline and is empty for a commit with no note; both
    /// have to come out as a commit, the second with no records.
    #[test]
    fn a_log_parses_noted_and_unnoted_commits() {
        // As `git log -z` writes it: NUL after every field, including the
        // last, and a newline ending a note that exists.
        let out = format!(
            "aaa\u{0}2026-01-02T00:00:00+00:00\u{0}second\u{0}{LINE}\n\u{0}\
             bbb\u{0}2026-01-01T00:00:00+00:00\u{0}first\u{0}\u{0}"
        );
        let log = read_log(out.as_bytes(), |_| false).unwrap().0;
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].sha, "aaa");
        assert_eq!(log[0].subject, "second");
        assert_eq!(log[0].records.len(), 1);
        assert_eq!(log[1].sha, "bbb");
        assert!(log[1].records.is_empty());
    }

    /// The byte the first version of this format used as its separator.
    #[test]
    fn a_control_character_in_a_subject_stays_in_the_subject() {
        let out = format!("aaa\u{0}2026-01-02T00:00:00+00:00\u{0}odd \u{1e} one\u{0}{LINE}\n\u{0}");
        let log = read_log(out.as_bytes(), |_| false).unwrap().0;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].subject, "odd \u{1e} one");
        assert_eq!(log[0].records.len(), 1);
    }

    /// Stopping is the whole saving, so nothing after the accepted commit may
    /// be read: here the rest of the stream fails if it is touched.
    #[test]
    fn a_log_stops_reading_at_the_commit_that_satisfies_it() {
        struct Poisoned;
        impl Read for Poisoned {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("read past the stop"))
            }
        }
        let head = format!(
            "aaa\u{0}2026-01-02T00:00:00+00:00\u{0}second\u{0}\u{0}\
             bbb\u{0}2026-01-01T00:00:00+00:00\u{0}first\u{0}{LINE}\n\u{0}"
        );
        // A one-byte buffer, so the reader cannot have prefetched the poison
        // by accident and still pass.
        let out = BufReader::with_capacity(1, head.as_bytes().chain(Poisoned));
        let (log, stopped) = read_log(out, |c| !c.records.is_empty()).unwrap();
        assert!(stopped);
        assert_eq!(log.len(), 2);
        assert_eq!(log[1].sha, "bbb");

        let out = BufReader::new(head.as_bytes().chain(Poisoned));
        assert!(read_log(out, |_| false).is_err());
    }
}
