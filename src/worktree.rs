//! Throwaway checkouts of historical commits, for `tak backfill --commits`.
//!
//! Each commit is built and measured in a detached `git worktree` under an
//! owner-only temporary directory, never in the user's own checkout: a build
//! that rewrites `target/` or a fixture there would leave the working tree in
//! the state of whatever commit was measured last. Worktrees share the
//! repository's object store and refs, so a checkout costs only the files it
//! writes, and a note recorded from the main checkout lands on the same
//! `refs/notes/tak`.
//!
//! Git is a subprocess here for the same reason it is in [`crate::notes`].

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

fn git(args: &[&std::ffi::OsStr]) -> Result<String> {
    let out = Command::new("git").args(args).output().with_context(|| {
        format!(
            "failed to run `git {}`",
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        )
    })?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn git_str(args: &[&str]) -> Result<String> {
    let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    git(&args)
}

/// First-parent commits in `range`, newest first.
///
/// First-parent for the reason [`crate::notes::rev_list`] gives: the commits
/// on a merged branch are not points on the trunk's timeline, and CI never
/// recorded them as such.
///
/// `range` is anything `git rev-list` accepts as one argument, usually
/// `A..B`. One starting with `-` is refused rather than handed to git, where
/// it would be read as an option.
pub fn first_parent_commits(range: &str) -> Result<Vec<String>> {
    if range.starts_with('-') || range.trim().is_empty() {
        bail!("not a commit range: {range:?} (try `main~20..main`)");
    }
    // `--` so a range that happens to match a file name is never read as a
    // path filter, which would silently drop every commit not touching it.
    let out = git_str(&["rev-list", "--first-parent", range, "--"])
        .with_context(|| format!("could not list the commits in {range}"))?;
    Ok(out.lines().map(str::to_string).collect())
}

/// When `sha` was committed, as seconds since the epoch, and its subject line.
pub fn describe(sha: &str) -> Result<(u64, String)> {
    let out = git_str(&["show", "-s", "--format=%ct %s", sha])?;
    let (ts, subject) = out.split_once(' ').unwrap_or((out.as_str(), ""));
    let ts = ts
        .trim()
        .parse()
        .with_context(|| format!("unexpected commit time for {sha}: {ts:?}"))?;
    Ok((ts, subject.to_string()))
}

/// The root of the work tree containing `dir`.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    let out = git(&[
        "-C".as_ref(),
        dir.as_os_str(),
        "rev-parse".as_ref(),
        "--show-toplevel".as_ref(),
    ])
    .context("not inside a git repository")?;
    Ok(PathBuf::from(out))
}

/// Forget worktrees whose directories no longer exist.
///
/// A run killed outright — SIGKILL, or a signal during a step that handles
/// its own, like a subject's `version_cmd` — cannot remove its checkout. The
/// directory is in the system temporary directory and goes with it; this
/// clears the bookkeeping git keeps for it, so stale entries do not pile up
/// in `git worktree list`.
pub fn prune() {
    let _ = git_str(&["worktree", "prune"]);
}

/// Worktrees this process has created and not yet removed, for the interrupt
/// handler to clean up. Held for the whole of [`Worktree::add`], so a
/// checkout being created while an interrupt arrives is either finished and
/// listed here, or never started.
static LIVE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// Set once an interrupt is being handled: nothing new may be checked out.
static STOPPING: AtomicBool = AtomicBool::new(false);

/// A detached checkout of one commit, removed when dropped.
pub struct Worktree {
    path: PathBuf,
}

impl Worktree {
    /// Check `sha` out at `path`, which must not exist yet.
    ///
    /// `hooks` is an empty directory used as `core.hooksPath`: `worktree add`
    /// runs the repository's `post-checkout` hook, and a project hook that
    /// installs dependencies or regenerates files would run at every commit
    /// of the backfill, unasked, and change what the build starts from.
    pub fn add(path: &Path, sha: &str, hooks: &Path) -> Result<Worktree> {
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        if STOPPING.load(Ordering::SeqCst) {
            bail!("interrupted");
        }
        let mut hooks_cfg = std::ffi::OsString::from("core.hooksPath=");
        hooks_cfg.push(hooks);
        git(&[
            "-c".as_ref(),
            &hooks_cfg,
            "worktree".as_ref(),
            "add".as_ref(),
            "--quiet".as_ref(),
            "--detach".as_ref(),
            path.as_os_str(),
            sha.as_ref(),
        ])
        .with_context(|| format!("could not check out {sha}"))?;
        live.push(path.to_path_buf());
        Ok(Worktree {
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Remove a checkout and git's record of it.
///
/// `--force` because the build has just filled it with untracked output.
/// Falling back to deleting the directory and pruning covers a worktree git
/// no longer recognises, so the temporary directory is emptied either way.
fn remove(path: &Path) {
    let ok = git(&[
        "worktree".as_ref(),
        "remove".as_ref(),
        "--force".as_ref(),
        path.as_os_str(),
    ])
    .is_ok();
    if !ok || path.exists() {
        let _ = std::fs::remove_dir_all(path);
        prune();
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        remove(&self.path);
        live.retain(|p| p != &self.path);
    }
}

/// Remove this process's worktrees and `scratch` when tak is interrupted.
///
/// Destructors do not run when a signal ends the process, and without this a
/// Ctrl-C during a long build leaves a full checkout in the temporary
/// directory and a stale entry in `git worktree list` for every interrupted
/// run.
///
/// The handler only writes the signal number to a socket; a thread blocked on
/// the other end does the cleanup, since running git from inside a signal
/// handler is not safe. The build and measured commands share tak's process
/// group, so a terminal's Ctrl-C has already reached them by the time the
/// thread removes the directory they were running in.
///
/// While a subject's `version_cmd` runs, [`crate::measure`] installs its own
/// handlers, which stop tak at once; [`prune`] at the start of the next run
/// tidies up after that.
#[cfg(unix)]
pub fn clean_up_on_interrupt(scratch: &Path) -> Result<()> {
    use std::io::Read;
    use std::os::fd::IntoRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicI32;

    static WRITE_FD: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn on_signal(sig: libc::c_int) {
        let fd = WRITE_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            let byte = sig as u8;
            // SAFETY: write is async-signal-safe; the buffer outlives the call.
            unsafe {
                libc::write(fd, (&byte as *const u8).cast(), 1);
            }
        }
    }

    // A socket pair rather than `pipe`, because std creates it close-on-exec
    // on every Unix: the build and the measured commands must not inherit
    // either end.
    let (mut rx, tx) = UnixStream::pair().context("could not set up interrupt handling")?;
    WRITE_FD.store(tx.into_raw_fd(), Ordering::SeqCst);
    let scratch = scratch.to_path_buf();
    std::thread::spawn(move || {
        let mut sig = [0u8; 1];
        if rx.read_exact(&mut sig).is_err() {
            return;
        }
        STOPPING.store(true, Ordering::SeqCst);
        // Never released: the process exits while holding it, so nothing
        // can be checked out after the sweep.
        let live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        for p in live.iter() {
            remove(p);
        }
        let _ = std::fs::remove_dir_all(&scratch);
        eprintln!("\n  interrupted — removed backfill worktrees");
        std::process::exit(128 + i32::from(sig[0]));
    });
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: plain-data struct, filled in before use; the handler is an
        // `extern "C" fn(c_int)`, as sa_sigaction expects without SA_SIGINFO.
        // SA_RESTART so a wait on the build is not cut short by the signal.
        unsafe {
            let mut new: libc::sigaction = std::mem::zeroed();
            new.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            new.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut new.sa_mask);
            libc::sigaction(sig, &new, std::ptr::null_mut());
        }
    }
    Ok(())
}

/// Windows has no equivalent wired up yet: an interrupted run leaves its
/// checkout for [`prune`] and the temporary directory's own cleanup.
#[cfg(not(unix))]
pub fn clean_up_on_interrupt(_scratch: &Path) -> Result<()> {
    Ok(())
}
