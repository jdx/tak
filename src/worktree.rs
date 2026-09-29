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

/// When `sha` was committed, as seconds since the epoch, read from the raw
/// commit object.
///
/// Not `git show --format=%ct`, for two reasons. With `log.showSignature`
/// set, `show` writes a signature's verification text onto stdout ahead of
/// the format. And a commit dated before 1970, which only an import or a
/// hand-written object can produce, has a negative timestamp that `%ct`
/// prints as nothing at all. `cat-file` prints the object as stored, so the
/// committer line says exactly what the commit says.
pub fn commit_time(sha: &str) -> Result<i64> {
    let out = git_str(&["cat-file", "commit", sha])?;
    parse_commit_time(&out).with_context(|| format!("no readable committer date in {sha}"))
}

/// The timestamp on the `committer` header: `committer NAME <EMAIL> TS TZ`.
/// Only headers are searched, which end at the first blank line, so a
/// message line starting with `committer` cannot be taken for it. Taken from
/// the right, since the name and email may hold spaces.
fn parse_commit_time(object: &str) -> Option<i64> {
    let line = object
        .lines()
        .take_while(|l| !l.is_empty())
        .find_map(|l| l.strip_prefix("committer "))?;
    let mut fields = line.rsplit(' ');
    let _tz = fields.next()?;
    fields.next()?.parse().ok()
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

/// The running build's process group, or 0 when none is running.
///
/// The build gets a group of its own so that stopping tak stops all of it,
/// compilers and linkers included, even when the signal was sent to tak
/// alone: CI cancelling a job, or `kill <pid>`, reaches tak but not its
/// children. A terminal's Ctrl-C no longer reaches the build directly either,
/// since it is out of the foreground group; the interrupt handler kills the
/// group on tak's behalf.
#[cfg(unix)]
static BUILD_GROUP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// The build has been spawned as the leader of process group `pid`.
///
/// If an interrupt is already being handled, the handler may have looked for
/// a group before this one was recorded, so the build is killed here. Each
/// side writes its own flag before reading the other's, all `SeqCst`, so at
/// least one of them sees the group.
#[cfg(unix)]
pub fn build_started(pid: u32) {
    BUILD_GROUP.store(pid as i32, Ordering::SeqCst);
    if STOPPING.load(Ordering::SeqCst) {
        kill_build();
    }
}

/// Whether an interrupt is being handled. A build that fails from here on
/// was killed by tak, not broken by its commit.
pub fn stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

/// The build has been reaped; its group id may be reused from here on.
#[cfg(unix)]
pub fn build_finished() {
    BUILD_GROUP.store(0, Ordering::SeqCst);
}

/// SIGKILL rather than SIGTERM: the checkout it is writing into is about to
/// be deleted, and a build given time to clean up would only race that.
#[cfg(unix)]
fn kill_build() {
    let pgid = BUILD_GROUP.load(Ordering::SeqCst);
    if pgid > 0 {
        // SAFETY: killpg has no memory-safety preconditions.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
}

/// Commits whose build failed, each with the build it failed under.
///
/// Without a memory of these, a commit that can never build is chosen again
/// at the top of every run, and with `--limit` a backfill stops making
/// progress at the first one. Keyed on the `[build]` definition as well as
/// the commit, so changing the build — usually to fix exactly that failure —
/// tries every such commit again. Kept in git's common directory: shared by
/// the repository's worktrees, never committed, and local to this clone,
/// since a failure says as much about this machine's toolchain as about the
/// commit.
///
/// Only build failures are remembered. A measurement can fail for reasons
/// that do not repeat, such as a flaky `check`, and skipping that commit for
/// good would be the wrong answer to a transient problem.
pub struct FailedBuilds {
    path: PathBuf,
    build: String,
    shas: std::collections::BTreeSet<String>,
}

impl FailedBuilds {
    /// The commits that failed under `build`, a stable one-line description
    /// of the `[build]` definition.
    pub fn load(build: String) -> Result<FailedBuilds> {
        let common = git_str(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let path = PathBuf::from(common)
            .join("tak")
            .join("backfill-build-failed");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let shas = text
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .filter(|(_, b)| *b == build)
            .map(|(sha, _)| sha.to_string())
            .collect();
        Ok(FailedBuilds { path, build, shas })
    }

    pub fn contains(&self, sha: &str) -> bool {
        self.shas.contains(sha)
    }

    /// Remember that `sha` failed to build.
    pub fn add(&mut self, sha: &str) -> Result<()> {
        if !self.shas.insert(sha.to_string()) {
            return Ok(());
        }
        let line = format!("{sha}\t{}", self.build);
        self.rewrite(|lines| lines.push(line))
    }

    /// Forget `sha` under this build, once it has built.
    pub fn remove(&mut self, sha: &str) -> Result<()> {
        if !self.shas.remove(sha) {
            return Ok(());
        }
        let line = format!("{sha}\t{}", self.build);
        self.rewrite(|lines| lines.retain(|l| *l != line))
    }

    /// Re-read, change and replace the file.
    ///
    /// Re-read rather than written from this process's own view, so entries
    /// another backfill added since this one loaded are kept. Replaced by
    /// renaming a complete temporary file over it, so a concurrent reader,
    /// or a run killed mid-write, never sees half a file. Two backfills
    /// writing at the same instant can still lose one's entry. There is no
    /// lock, because the cost of that is small: a commit that failed to build
    /// gets built once more.
    fn rewrite(&self, change: impl FnOnce(&mut Vec<String>)) -> Result<()> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        change(&mut lines);
        lines.sort();
        lines.dedup();
        let dir = self.path.parent().context("no directory for build state")?;
        std::fs::create_dir_all(dir)?;
        let mut body = lines.join("\n");
        body.push('\n');
        let mut tmp = tempfile::NamedTempFile::new_in(dir)
            .with_context(|| format!("could not write in {}", dir.display()))?;
        std::io::Write::write_all(&mut tmp, body.as_bytes())?;
        tmp.persist(&self.path)
            .with_context(|| format!("could not write {}", self.path.display()))?;
        Ok(())
    }
}

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

    /// Check that `p`, symlinks resolved, is inside this checkout.
    ///
    /// Refusing `..` in the configured path is not enough: an old commit can
    /// hold a symlink where the current tak.toml expects a directory, and one
    /// pointing at the live tree would build or measure that tree at every
    /// commit of the backfill.
    pub fn check_contains(&self, p: &Path) -> Result<()> {
        let shown = p.strip_prefix(&self.path).unwrap_or(p).display();
        let real = p
            .canonicalize()
            .with_context(|| format!("{shown} does not exist at this commit"))?;
        if !real.starts_with(self.path.canonicalize()?) {
            bail!("{shown} leads outside the checkout, to {}", real.display());
        }
        Ok(())
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
/// handler is not safe. It kills the build's process group first (see
/// [`BUILD_GROUP`]), so a build never outlives the checkout it was writing
/// into. Measured commands share tak's own group and get a terminal's Ctrl-C
/// directly.
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

    // Once per process. The write end is deliberately never closed: the
    // handler may fire at any moment until tak exits, and closing it would
    // leave the handler writing to a descriptor that has been reused. A
    // second call would replace it, leaking the first and starting a second
    // thread, so it is a no-op instead.
    if WRITE_FD.load(Ordering::SeqCst) >= 0 {
        return Ok(());
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
        kill_build();
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

#[cfg(test)]
mod tests {
    use super::parse_commit_time;

    #[test]
    fn a_commit_time_is_the_committer_header_timestamp() {
        let object = "tree abc\nparent def\n\
                      author A Person <a@x> 1600000000 +0200\n\
                      committer The Committer <c@x> 1700000000 -0700\n\n\
                      subject\n";
        assert_eq!(parse_commit_time(object), Some(1_700_000_000));
        let before_1970 = "tree abc\ncommitter T <t@x> -14182940 +0000\n\nv2\n";
        assert_eq!(parse_commit_time(before_1970), Some(-14_182_940));
    }

    /// A signed commit carries a multi-line `gpgsig` header, and a message
    /// can say anything; neither may be read as the committer line.
    #[test]
    fn signatures_and_messages_are_not_the_committer_line() {
        let object = "tree abc\n\
                      committer T <t@x> 1700000000 +0000\n\
                      gpgsig -----BEGIN SSH SIGNATURE-----\n \
                      U1NIU0lH 1234 +0000\n -----END SSH SIGNATURE-----\n\n\
                      committer X <x@x> 1 +0000\n";
        assert_eq!(parse_commit_time(object), Some(1_700_000_000));
        let no_header = "tree abc\n\ncommitter X <x@x> 1 +0000\n";
        assert_eq!(parse_commit_time(no_header), None);
    }
}
