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
use std::process::Command;

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
