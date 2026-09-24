//! Measurement backends.
//!
//! Two tiers, deliberately separated:
//!
//! - **Deterministic** (`instructions`) — reproducible to ~0.02% run-to-run and
//!   ~0.035% across wildly different machine load. This is the only tier that may
//!   gate CI.
//! - **Timing** (`wall_*`) — recorded and charted, never gated. On a quiet 32-core
//!   host wall clock still shows 4–20% coefficient of variation; under contention
//!   the median moves ~150%.
//!
//! Syscall counts and peak RSS sit awkwardly between the two: better than wall
//! clock (~1%) but not deterministic, because they move with thread scheduling.
//! They are recorded, and may be flagged, but must not gate at a tight threshold.

use crate::config::{AutoRuns, DEFAULT_OK_EXIT_CODES, Runs, SELF_TOOL, Subject};
use crate::settings::Settings;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct Plan {
    pub cmd: Vec<String>,
    pub warmup: u32,
    pub runs: u32,
    /// Directory to run in. Declared benchmarks resolve relative commands
    /// against `tak.toml`'s directory, not the caller's — otherwise the same
    /// benchmark measures different things depending on where you stood.
    pub dir: Option<std::path::PathBuf>,
    /// Resolved settings. Carried on the plan rather than read from a global so
    /// a test can measure under a different configuration without touching the
    /// environment of the whole test binary.
    pub settings: Settings,
}

/// A command for running a benchmark subject, with the scrubbed variables
/// removed.
///
/// Every subject spawn goes through here. Under cachegrind the removal is
/// applied to valgrind itself, which the subject inherits from.
///
/// What gets removed is [`Settings::scrubbed_env`] — `env_deny` less
/// `env_allow`, both declared on the `Settings` registry. The default is the
/// forge tokens because a CLI that finds one often does more with it than
/// without, so a measurement would move depending on whether CI happened to
/// export one.
///
/// This controls direct inheritance, not hostile-code isolation. A subject can
/// still inspect accessible same-user processes and files, so `backfill` must
/// run without credentials when its release binaries are not trusted.
///
/// tak's own network calls are unaffected — `backfill` authenticates with
/// `curl` directly rather than through this path.
fn subject(bin: &str, settings: &Settings) -> Command {
    let mut c = Command::new(bin);
    for key in settings.scrubbed_env() {
        c.env_remove(key);
    }
    c
}

/// Where a command runs and what it sees, beyond its argv.
#[derive(Debug, Clone, Copy)]
struct Site<'a> {
    dir: Option<&'a Path>,
    env: &'a BTreeMap<String, String>,
    settings: &'a Settings,
}

/// Build a spawn of `argv` at `site`.
///
/// Declared variables are applied after the scrub. A variable written into
/// `tak.toml` for a subject is an explicit choice, not something inherited by
/// accident, which is the only thing the scrub exists to stop.
fn command(argv: &[String], site: &Site) -> Result<Command> {
    let (bin, args) = argv.split_first().context("empty command")?;
    let mut c = subject(bin, site.settings);
    c.args(args).envs(site.env);
    if let Some(d) = site.dir {
        c.current_dir(d);
    }
    Ok(c)
}

/// One successful run of a subject's timed command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Elapsed wall time in milliseconds.
    pub ms: f64,
    /// What the command exited with: one of its `ok_exit_codes`, kept so the
    /// export shows what actually happened rather than assuming 0.
    pub exit_code: i32,
}

/// The exit code of a sample that counts, or the error that drops its
/// subject.
///
/// A death by signal has no exit code and always fails: a subject that
/// crashed or was killed did not do the work being measured, whatever its
/// `ok_exit_codes` allow.
fn accepted(bin: &str, status: std::process::ExitStatus, ok: &[i32], via: &str) -> Result<i32> {
    match status.code() {
        Some(code) if ok.contains(&code) => Ok(code),
        // Name the accepted codes only when they are not the default, where
        // "exited with 1" alone would leave the reader wondering why that
        // was a failure.
        _ if ok == DEFAULT_OK_EXIT_CODES => {
            bail!("benchmark subject `{bin}` exited with {status}{via}")
        }
        _ => bail!(
            "benchmark subject `{bin}` exited with {status}{via}, and ok_exit_codes is {}",
            ok.iter().map(i32::to_string).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Run once, discarding output, returning elapsed wall time and exit code.
///
/// No shell. Spawning a shell adds its own startup cost and variance to every
/// sample, which for commands in the 10ms range is a large fraction of the
/// measurement — the same reasoning behind poop's refusal to support one.
fn time_once(cmd: &[String], site: &Site, ok: &[i32]) -> Result<Sample> {
    let mut c = command(cmd, site)?;
    c.stdout(Stdio::null()).stderr(Stdio::null());
    let bin = &cmd[0];
    let start = Instant::now();
    let status = c
        .status()
        .with_context(|| format!("failed to spawn `{bin}`"))?;
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let exit_code = accepted(bin, status, ok, "")?;
    Ok(Sample { ms, exit_code })
}

/// How much of a failing prepare, setup or check step's stderr to keep for
/// the message. Enough for the last few lines; a verbose reset command's full
/// output would otherwise sit in memory before every sample.
const UNTIMED_STDERR_TAIL: usize = 4096;

/// Run a subject's prepare step. Untimed, so reading its stderr for the error
/// message costs the measurement nothing.
fn prepare_once(cmd: &[String], site: &Site) -> Result<()> {
    required("prepare", cmd, site)
}

/// Run an untimed step that must succeed: `prepare` or `setup`.
fn required(what: &str, cmd: &[String], site: &Site) -> Result<()> {
    match untimed(what, cmd, site)? {
        None => Ok(()),
        Some(failure) => bail!("{failure}"),
    }
}

/// Run a subject's check step after a timed sample. `Ok(None)` is a pass and
/// `Ok(Some(why))` a failed sample. Only a check that cannot be started at
/// all is an error: that is a mistake in `tak.toml`, not a finding about the
/// subject, and would otherwise read as every sample having failed.
fn check_once(cmd: &[String], site: &Site) -> Result<Option<String>> {
    untimed("check", cmd, site)
}

/// Run an untimed step — `prepare`, `setup` or `check`, named by `step` in
/// messages. `Ok(Some(why))` when it exits non-zero, naming the step and the
/// last line of its stderr.
///
/// Its output is not passed through: on a terminal it would tear up the
/// progress bar, and in a log a setup that clones a fixture could bury the
/// results. The tail of stderr is kept for the message.
///
/// Always judged by exit 0, whatever the subject's `ok_exit_codes` say: those
/// describe the program being measured. A setup or reset that failed leaves
/// every later sample starting from the wrong state, and a check passes only
/// by exiting 0.
fn untimed(step: &str, cmd: &[String], site: &Site) -> Result<Option<String>> {
    use std::io::Read;

    let mut c = command(cmd, site)?;
    let bin = &cmd[0];
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn {step} `{bin}`"))?;
    // Keep only the tail, reading as it arrives so the child never blocks on a
    // full pipe.
    let mut tail: Vec<u8> = Vec::new();
    if let Some(mut err) = child.stderr.take() {
        let mut buf = [0u8; 8192];
        loop {
            let n = err.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > UNTIMED_STDERR_TAIL {
                tail.drain(..tail.len() - UNTIMED_STDERR_TAIL);
            }
        }
    }
    let status = child
        .wait()
        .with_context(|| format!("failed to wait for {step} `{bin}`"))?;
    if status.success() {
        return Ok(None);
    }
    // A check like `git diff --quiet` says nothing on failure by design, so
    // silence is reported as the status alone.
    let stderr = String::from_utf8_lossy(&tail);
    Ok(Some(match stderr.lines().rfind(|l| !l.trim().is_empty()) {
        Some(last) => format!("{step} `{bin}` exited with {status}: {}", last.trim()),
        None => format!("{step} `{bin}` exited with {status}"),
    }))
}

/// One entry in a run's sample order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Index into the subjects being measured.
    pub subject: usize,
    /// Whether this sample is kept. Warmup samples run through the same
    /// prepare-then-run path and are discarded.
    pub timed: bool,
}

/// The order in which to take samples: every warmup round, then every timed
/// round, each round a fresh shuffle of the subjects due in it.
///
/// Measuring one subject to completion before starting the next puts any
/// drift over the run — contention on a shared host, thermal throttling, a
/// cache filling up — entirely on whichever subjects ran while it happened,
/// where it reads as a difference between them. One sample of each per round
/// spreads that drift across all of them, and shuffling each round stops any
/// subject from systematically running first (cold) or right after a
/// particular other one (whose leftovers it inherits).
///
/// A subject with fewer samples than there are rounds sits out evenly spaced
/// rounds rather than dropping out at the end, which would leave the late
/// part of the run — and its drift — to the others.
///
/// `counts` is `(warmup, runs)` per subject. The same seed gives the same
/// order for the same subjects.
pub fn schedule(counts: &[(u32, u32)], seed: u64) -> Vec<Slot> {
    let mut rng = fastrand::Rng::with_seed(seed);
    let warmups: Vec<u64> = counts.iter().map(|&(w, _)| u64::from(w)).collect();
    let runs: Vec<u64> = counts.iter().map(|&(_, r)| u64::from(r)).collect();
    let mut out = rounds(&warmups, false, &mut rng);
    out.extend(rounds(&runs, true, &mut rng));
    out
}

/// Shuffled rounds taking `count[i]` samples of subject `i`.
fn rounds(count: &[u64], timed: bool, rng: &mut fastrand::Rng) -> Vec<Slot> {
    let total = count.iter().copied().max().unwrap_or(0);
    let mut out = Vec::new();
    for round in 0..total {
        // Subject i is due in the rounds where floor(r * k / R) steps up:
        // exactly k of the R rounds, as evenly spaced as integers allow.
        let mut due: Vec<usize> = (0..count.len())
            .filter(|&i| (round + 1) * count[i] / total > round * count[i] / total)
            .collect();
        rng.shuffle(&mut due);
        out.extend(due.into_iter().map(|subject| Slot { subject, timed }));
    }
    out
}

/// Derive a benchmark's seed from the run's, so `--bench x --seed n` takes
/// the same order for `x` as the full run with `--seed n` did.
pub fn seed_for(seed: u64, bench: &str) -> u64 {
    // FNV-1a: a fixed function, unlike std's randomly keyed hasher.
    bench.bytes().fold(seed ^ 0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Told about a run as it happens, to report progress.
pub trait Observer {
    /// Samples still to take per subject, warmups included. Called before the
    /// first sample and again once `runs = "auto"` has settled its counts.
    fn planned(&mut self, _remaining: &[u64]) {}
    /// `subject`'s setup step is starting. Untimed and not a sample, so it
    /// counts toward neither the samples taken nor their average.
    fn setting_up(&mut self, _subject: usize) {}
    /// A sample of `subject` is starting.
    fn started(&mut self, _subject: usize) {}
    /// A sample of `subject` finished; `elapsed` covers its prepare step too.
    fn finished(&mut self, _subject: usize, _elapsed: Duration) {}
    /// `subject` failed and takes no more samples.
    fn dropped(&mut self, _subject: usize) {}
}

/// An observer that reports nothing.
pub struct Quiet;
impl Observer for Quiet {}

/// What one subject's run produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Samples {
    /// Timed samples in milliseconds, in the order taken.
    pub times: Vec<f64>,
    /// Whether the subject's `check` passed after each timed sample, aligned
    /// with `times`. Empty when the subject has no check.
    pub checks: Vec<bool>,
    /// What each timed sample exited with, aligned with `times`: always one
    /// of the subject's `ok_exit_codes`.
    pub exit_codes: Vec<i32>,
    /// Why the first failing check failed, for the warning that reports it.
    pub first_failure: Option<String>,
}

impl Samples {
    /// Checks that passed.
    pub fn passed(&self) -> usize {
        self.checks.iter().filter(|&&ok| ok).count()
    }
}

/// Measure several subjects against each other, interleaved.
///
/// Returns each subject's timed samples in the order they were taken, with
/// the outcome of its check after each one, or the error that stopped it. A failing subject is dropped from the remaining
/// rounds and the others carry on: one competitor breaking should not throw
/// away a long run's worth of everyone else's samples. Subject directories
/// are used as given, so the caller resolves them first.
///
/// Every subject's `setup` runs first, once, in the order given: before
/// any warmup, so a subject's first sample never shares the machine with
/// another's setup and none of it is counted toward an auto run count. A
/// subject whose setup fails is dropped like one whose prepare does.
///
/// Sampling then runs in two phases. The warmups come first, plus one kept
/// sample of every `runs = "auto"` subject that has no warmups, so each auto
/// subject has been timed at least once. Its run count is then fixed from the
/// fastest of those samples, and every remaining timed sample is taken in
/// shuffled rounds.
///
/// A subject's `check` runs after each of its timed samples, pilots included,
/// and never after a warmup: a warmup is discarded, so whether it did the
/// right work says nothing about any result. A failing check is recorded
/// against its sample and the subject carries on — how often it fails is the
/// finding — so only a check that cannot be started drops the subject.
pub fn interleaved(
    subjects: &[Subject],
    seed: u64,
    settings: &Settings,
    observer: &mut dyn Observer,
) -> Vec<Result<Samples>> {
    interleaved_with_versions(subjects, seed, settings, observer).0
}

/// A subject's `version_cmd` result: `None` when it declares none or never
/// got that far, otherwise what it printed or why it could not tell.
pub type Version = Option<Result<String>>;

/// [`interleaved`], also returning each subject's version.
///
/// Versions are asked for after every `setup` and before any warmup: setup
/// may be what installs or builds the subject, so asking earlier would name
/// the wrong program or none, and a tool that warms a cache or checks for
/// updates on `--version` does so before the first sample rather than
/// between samples. A subject whose setup failed is not asked.
pub fn interleaved_with_versions(
    subjects: &[Subject],
    seed: u64,
    settings: &Settings,
    observer: &mut dyn Observer,
) -> (Vec<Result<Samples>>, Vec<Version>) {
    let mut rng = fastrand::Rng::with_seed(seed);
    let mut results: Vec<Result<Samples>> = subjects
        .iter()
        .map(|s| {
            // Zero runs would leave nothing to report on.
            if s.runs == Runs::Fixed(0) {
                bail!("runs must be at least 1");
            }
            Ok(Samples::default())
        })
        .collect();
    // The fastest whole sample of each subject so far, prepare included —
    // what an auto count is sized from. The fastest, because a warmup is
    // often a cold outlier and would shrink the count for no reason.
    let mut fastest: Vec<Option<Duration>> = vec![None; subjects.len()];

    let warmups: Vec<u64> = subjects.iter().map(|s| u64::from(s.warmup)).collect();
    let pilots: Vec<u64> = subjects
        .iter()
        .map(|s| u64::from(s.runs == Runs::Auto && s.warmup == 0))
        .collect();
    // Until the auto counts are known, plan on each one's floor.
    let provisional: Vec<u64> = subjects
        .iter()
        .zip(&warmups)
        .map(|(s, w)| {
            w + u64::from(match s.runs {
                Runs::Fixed(n) => n,
                Runs::Auto => s.auto.min,
            })
        })
        .collect();
    observer.planned(&provisional);

    for (i, s) in subjects.iter().enumerate() {
        let (Some(setup), Ok(_)) = (&s.setup, &results[i]) else {
            continue;
        };
        observer.setting_up(i);
        let site = Site {
            dir: s.setup_dir.as_deref(),
            env: &s.env,
            settings,
        };
        if let Err(e) = required("setup", setup, &site) {
            results[i] = Err(e);
            observer.dropped(i);
        }
    }

    let versions: Vec<Version> = subjects
        .iter()
        .zip(&results)
        .map(|(s, r)| r.as_ref().ok().and_then(|_| subject_version(s, settings)))
        .collect();

    let mut first = rounds(&warmups, false, &mut rng);
    first.extend(rounds(&pilots, true, &mut rng));
    run_slots(
        &first,
        subjects,
        settings,
        &mut results,
        &mut fastest,
        observer,
    );

    let remaining: Vec<u64> = subjects
        .iter()
        .enumerate()
        .map(|(i, s)| match (&results[i], s.runs) {
            (Err(_), _) => 0,
            (Ok(_), Runs::Fixed(n)) => u64::from(n),
            (Ok(taken), Runs::Auto) => {
                let n = auto_runs(&s.auto, fastest[i]);
                u64::from(n).saturating_sub(taken.times.len() as u64)
            }
        })
        .collect();
    observer.planned(&remaining);
    let second = rounds(&remaining, true, &mut rng);
    run_slots(
        &second,
        subjects,
        settings,
        &mut results,
        &mut fastest,
        observer,
    );
    (results, versions)
}

/// An auto subject's run count. One that was never timed — its every sample
/// failed, so it is being dropped anyway — gets the floor.
fn auto_runs(auto: &AutoRuns, fastest: Option<Duration>) -> u32 {
    fastest.map_or(auto.min, |d| auto.runs_for(d))
}

/// Take the given samples, recording timed ones and the fastest slot seen.
///
/// `fastest` covers prepare and the command but not the check, so an auto
/// count is sized from the same kind of sample whether it was a warmup (which
/// runs no check) or a pilot. The observer is told the whole slot, check
/// included, because that is what its time estimate has to cover.
fn run_slots(
    slots: &[Slot],
    subjects: &[Subject],
    settings: &Settings,
    results: &mut [Result<Samples>],
    fastest: &mut [Option<Duration>],
    observer: &mut dyn Observer,
) {
    for slot in slots {
        let Ok(samples) = &mut results[slot.subject] else {
            continue;
        };
        let s = &subjects[slot.subject];
        let site = Site {
            dir: s.dir.as_deref(),
            env: &s.env,
            settings,
        };
        observer.started(slot.subject);
        let began = Instant::now();
        let taken = s
            .prepare
            .as_deref()
            .map_or(Ok(()), |p| prepare_once(p, &site))
            .and_then(|()| time_once(&s.cmd, &site, &s.ok_exit_codes));
        let elapsed = began.elapsed();
        // Only after the clock has stopped: the check is outside the
        // measurement, however long it takes.
        let checked = match (&taken, &s.check) {
            (Ok(_), Some(check)) if slot.timed => check_once(check, &site).map(Some),
            _ => Ok(None),
        };
        match taken.and_then(|sample| checked.map(|c| (sample, c))) {
            Ok((sample, check)) => {
                if slot.timed {
                    samples.times.push(sample.ms);
                    samples.exit_codes.push(sample.exit_code);
                }
                if let Some(failure) = check {
                    samples.checks.push(failure.is_none());
                    if samples.first_failure.is_none() {
                        samples.first_failure = failure;
                    }
                }
                let f = &mut fastest[slot.subject];
                *f = Some(f.map_or(elapsed, |f| f.min(elapsed)));
                observer.finished(slot.subject, began.elapsed());
            }
            Err(e) => {
                results[slot.subject] = Err(e);
                observer.dropped(slot.subject);
            }
        }
    }
}

/// Wall-clock statistics over a set of samples, in milliseconds.
///
/// Reports `min` alongside the mean because contention is one-sided — a busy
/// machine can only make a run slower, never faster — so the minimum is a far
/// more robust estimator than the mean on shared CI hardware. The standard
/// deviation is there to show how noisy the run was, not to be compared.
pub fn stats(samples: &[f64]) -> BTreeMap<String, f64> {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let mean = sorted.iter().sum::<f64>() / n as f64;
    // Sample (n - 1) deviation, as hyperfine reports; a single sample has none.
    let stddev = if n > 1 {
        (sorted.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
    } else {
        0.0
    };
    BTreeMap::from([
        ("wall_min_ms".to_string(), sorted[0]),
        ("wall_p50_ms".to_string(), sorted[n / 2]),
        ("wall_mean_ms".to_string(), mean),
        ("wall_max_ms".to_string(), sorted[n - 1]),
        ("wall_stddev_ms".to_string(), stddev),
        ("wall_n".to_string(), n as f64),
    ])
}

/// Signs a subject's samples should not be taken at face value, as
/// sentences for stderr. `samples` must be in the order they were taken.
///
/// Two, both from hyperfine's experience of what goes wrong:
///
/// - **Outliers**, by modified z-score (0.6745 x distance from the median /
///   median absolute deviation) above 3.5, the usual cut-off. Something else
///   on the machine ran, or the command's own work varies from run to run.
/// - **A slow first sample**, over twice the median of the rest: a cache the
///   warmup did not fill. The first timed sample should look like the others.
///
/// Nothing is dropped or corrected; the point is to say when a comparison
/// deserves a second run. Slow outliers leave the minimum alone, but a fast
/// one may be the minimum, so the two are reported apart.
pub fn warnings(samples: &[f64]) -> Vec<String> {
    let mut out = Vec::new();
    let n = samples.len();
    if n < 5 {
        return out;
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let m = v.len();
        if m.is_multiple_of(2) {
            (v[m / 2 - 1] + v[m / 2]) / 2.0
        } else {
            v[m / 2]
        }
    };
    let med = median(&mut samples.to_vec());
    let mad = median(&mut samples.iter().map(|x| (x - med).abs()).collect());
    // The modified z-score needs a spread to divide by. When over half the
    // samples equal the median the median deviation is zero, and any
    // stand-in computed from all the samples would be inflated by the very
    // spikes it should catch. With a flat majority there is no noise to
    // measure against, so anything over a quarter away from the median
    // counts. Identical samples still raise nothing.
    let outlier = |x: f64| {
        if mad > 0.0 {
            0.6745 * (x - med).abs() / mad > 3.5
        } else {
            med > 0.0 && (x - med).abs() > 0.25 * med
        }
    };
    {
        let (fast, slow) = samples
            .iter()
            .filter(|&&x| outlier(x))
            .fold(
                (0, 0),
                |(f, s), &x| if x < med { (f + 1, s) } else { (f, s + 1) },
            );
        if slow > 0 {
            out.push(format!(
                "{slow} of {n} samples are slow outliers; something else was running, or the \
                 command's work varies. Consider a quieter machine or more runs."
            ));
        }
        // A fast outlier is not harmless the way a slow one is: it is the
        // minimum tak reports, so the headline number may rest on it.
        if fast > 0 {
            out.push(format!(
                "{fast} of {n} samples are fast outliers, and the reported minimum may be one of \
                 them; check the command did the same work every time."
            ));
        }
    }
    let rest = median(&mut samples[1..].to_vec());
    if rest > 0.0 && samples[0] > 2.0 * rest {
        out.push(format!(
            "the first sample took {:.1}x the median of the rest; the warmup did not fill some \
             cache. Consider more warmup runs.",
            samples[0] / rest
        ));
    }
    out
}

/// Wall-clock statistics over `plan.runs` samples of one command.
pub fn wall(plan: &Plan) -> Result<BTreeMap<String, f64>> {
    let subject = Subject {
        name: SELF_TOOL.to_string(),
        cmd: plan.cmd.clone(),
        prepare: None,
        setup: None,
        setup_dir: None,
        check: None,
        dir: plan.dir.clone(),
        version_cmd: None,
        env: BTreeMap::new(),
        vars: BTreeMap::new(),
        when: None,
        runs: Runs::Fixed(plan.runs),
        auto: AutoRuns {
            budget: crate::config::DEFAULT_BUDGET,
            min: crate::config::DEFAULT_MIN_RUNS,
            max: crate::config::DEFAULT_MAX_RUNS,
        },
        warmup: plan.warmup,
        counters: false,
        ok_exit_codes: DEFAULT_OK_EXIT_CODES.to_vec(),
    };
    // One subject has one possible order, so the seed is irrelevant.
    let samples = interleaved(
        std::slice::from_ref(&subject),
        0,
        &plan.settings,
        &mut Quiet,
    )
    .pop()
    .expect("one result per subject")?;
    Ok(stats(&samples.times))
}

/// Is cachegrind usable on this machine?
///
/// Exposed so callers can tell "no counters because valgrind is missing" from
/// "no counters because the measurement failed" — two problems with entirely
/// different fixes.
pub fn valgrind_available() -> bool {
    Command::new("valgrind")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Repeats of the cachegrind run. Three is enough to catch a bimodal subject
/// without tripling the cost of a measurement that is already ~50x slowed.
const COUNTER_RUNS: u32 = 3;

/// A subject varying by more than this is doing environment-dependent work, and
/// its instruction count is not a usable gate. Well above the ~0.005% observed
/// for a genuinely hermetic command, far below the 16% seen from one that
/// touches the network.
const SPREAD_WARN_PCT: f64 = 0.5;

/// Instruction counts from repeated cachegrind runs.
#[derive(Debug, Clone, Copy)]
pub struct Counted {
    pub min: u64,
    pub max: u64,
    pub runs: u32,
}

impl Counted {
    /// Relative spread across runs, as a percentage of the minimum.
    pub fn spread_pct(&self) -> f64 {
        if self.min == 0 {
            return 0.0;
        }
        (self.max - self.min) as f64 / self.min as f64 * 100.0
    }

    /// Whether this subject looks non-hermetic.
    ///
    /// The metric is deterministic; the program being measured need not be. A
    /// CLI that checks for updates, reads a cache it may have just created, or
    /// resolves DNS retires a different number of instructions depending on
    /// conditions that have nothing to do with the code under test.
    pub fn is_suspect(&self) -> bool {
        self.spread_pct() > SPREAD_WARN_PCT
    }
}

/// Instruction count via `valgrind --tool=cachegrind`, repeated.
///
/// Reports the **minimum**, for the same reason wall clock does: the extra work
/// a subject sometimes performs is one-sided. A run that consults the network or
/// populates a cache can only retire *more* instructions than the quiet path,
/// never fewer, so the floor is the stable estimator.
///
/// Returns `Ok(None)` when valgrind is unavailable rather than failing: this is
/// the expected state on macOS (no usable Apple Silicon support) and Windows.
/// Those platforms record timing only, and the CI gate lives on the Linux job.
/// Locally, a container gets you counters on any host.
pub fn instructions(
    cmd: &[String],
    dir: Option<&std::path::Path>,
    settings: &Settings,
) -> Result<Option<Counted>> {
    count(
        cmd,
        None,
        &Site {
            dir,
            env: &BTreeMap::new(),
            settings,
        },
        &DEFAULT_OK_EXIT_CODES,
    )
}

/// [`instructions`] for a declared subject: its environment, its
/// `ok_exit_codes`, and its prepare step before every cachegrind run, since
/// each run has to start from the same state the timed samples did.
pub fn subject_instructions(s: &Subject, settings: &Settings) -> Result<Option<Counted>> {
    count(
        &s.cmd,
        s.prepare.as_deref(),
        &Site {
            dir: s.dir.as_deref(),
            env: &s.env,
            settings,
        },
        &s.ok_exit_codes,
    )
}

/// Run a subject's `version_cmd` once and return the version it reports, or
/// `None` when the subject declares none.
///
/// It runs where the subject does — its directory, its environment, the same
/// scrub — because that decides which binary a bare name resolves to, and the
/// point is to name the program that was measured. The result is the first
/// non-empty line of stdout, or of stderr when stdout has none: `java
/// -version` and some older tools print only there. Only one line, because a
/// version is a label on a results page, and several tools follow it with a
/// licence or build banner.
pub fn subject_version(s: &Subject, settings: &Settings) -> Option<Result<String>> {
    let argv = s.version_cmd.as_ref()?;
    let site = Site {
        dir: s.dir.as_deref(),
        env: &s.env,
        settings,
    };
    Some(version_once(argv, &site, VERSION_TIMEOUT))
}

/// How much of each stream a `version_cmd` keeps. A version is its first
/// non-empty line; a tool that follows it with a banner, or a help dump,
/// must not be held in memory for the rest.
const VERSION_OUTPUT_CAP: usize = 8192;

/// How long a `version_cmd` may take. Asking a tool its version should be
/// instant; one that stalls on a network or licence check must cost a
/// warning, not hang the run before its first sample with nothing on
/// screen. Fixed rather than a setting until a real tool needs longer.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// After the command exits, how long to wait for its output to finish
/// arriving. Anything it wrote is already in the pipe by then, so this only
/// ends up mattering when a process it started in the background still
/// holds the pipe open — and that must not hold up the run.
const VERSION_OUTPUT_GRACE: Duration = Duration::from_millis(500);

/// Read `r` to the end, keeping the first `cap` bytes in `kept` and
/// discarding the rest. Draining it all means the writer never blocks on a
/// full pipe, so the command can finish and report its real exit status.
fn keep_prefix(mut r: impl std::io::Read, cap: usize, kept: &Mutex<Vec<u8>>) {
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let Ok(mut k) = kept.lock() else { return };
                let room = cap.saturating_sub(k.len());
                k.extend_from_slice(&buf[..n.min(room)]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Passing a fatal signal on to a running `version_cmd`'s process group.
///
/// The group is what lets a timeout stop everything the command started,
/// but it also takes the command out of the terminal's foreground group, so
/// a Ctrl-C — or a SIGTERM from CI cancelling the job — would stop tak and
/// leave the command running, however long it would otherwise have hung.
/// While a guard is alive, SIGINT, SIGTERM and SIGHUP kill that group and
/// then stop tak exactly as they would have without the handler. The
/// previous handlers come back when the guard drops, so the rest of a run
/// behaves as before: the measured commands share tak's group and get the
/// terminal's signals directly.
#[cfg(unix)]
mod forward_signals {
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// What the handler knows, as atomics alone: nothing else is safe to
    /// touch from a signal handler.
    ///
    /// A handler can run on any thread and be preempted anywhere, so every
    /// path that records a signal in `pending` must end with someone acting
    /// on it. All accesses are `SeqCst`, which puts them in one order:
    ///
    /// - The handler writes `pending`, then reads `done` and `pgid`.
    ///   [`State::spawned`] writes `pgid` and then takes `pending`;
    ///   [`State::end`] writes `done` and then takes `pending`. Either a take
    ///   comes after the handler's write, and the spawning thread acts on the
    ///   signal, or every take that could have collected it came before, and
    ///   so did the store preceding it, which the handler's later read sees,
    ///   so the handler acts on the signal itself.
    /// - The handler reads `spawning` before `pgid`, and `spawned` writes
    ///   them in the opposite order, so a handler that sees `spawning`
    ///   cleared also sees the group.
    ///
    /// A handler that starts after [`State::end`] sees `spawning` cleared and
    /// never writes `pending`; one that started earlier and was preempted
    /// sees `done` and stops tak itself, without killing a group whose id may
    /// have been reused by then.
    pub(super) struct State {
        /// Between installing the handlers and recording the group: a
        /// command may exist whose group the handler cannot know yet.
        spawning: AtomicBool,
        /// The group to kill, or 0 when there is none.
        pgid: AtomicI32,
        /// A signal that arrived while `spawning`, for the spawning thread
        /// to act on once the group is known.
        pending: AtomicI32,
        /// The version step is over and nothing will look at `pending`
        /// again, so a late handler must act on its signal itself.
        done: AtomicBool,
    }

    /// What the handler should do with a signal.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Act {
        /// Leave it for [`State::spawned`] or [`State::end`].
        Defer,
        /// Kill this group (0 for none) and stop tak.
        Die { pgid: i32 },
    }

    impl State {
        pub(super) const fn new() -> State {
            State {
                spawning: AtomicBool::new(false),
                pgid: AtomicI32::new(0),
                pending: AtomicI32::new(0),
                done: AtomicBool::new(true),
            }
        }

        pub(super) fn begin(&self) {
            self.pending.store(0, SeqCst);
            self.pgid.store(0, SeqCst);
            self.done.store(false, SeqCst);
            self.spawning.store(true, SeqCst);
        }

        /// The handler's decision.
        pub(super) fn on_signal(&self, sig: libc::c_int) -> Act {
            if self.wants_to_defer() {
                self.defer(sig)
            } else {
                Act::Die {
                    pgid: self.pgid.load(SeqCst),
                }
            }
        }

        /// First half of [`State::on_signal`]: still spawning, group not yet
        /// known. Separate so a test can pause a handler between the halves.
        fn wants_to_defer(&self) -> bool {
            let spawning = self.spawning.load(SeqCst);
            spawning && self.pgid.load(SeqCst) == 0
        }

        /// Second half: record the signal, then check whether whoever would
        /// have collected it already has.
        fn defer(&self, sig: libc::c_int) -> Act {
            self.pending.store(sig, SeqCst);
            if self.done.load(SeqCst) {
                return Act::Die { pgid: 0 };
            }
            match self.pgid.load(SeqCst) {
                0 => Act::Defer,
                pgid => Act::Die { pgid },
            }
        }

        /// Record the group once the spawn returns (`None` if it failed),
        /// and hand back any signal that arrived meanwhile with the group
        /// to kill for it.
        pub(super) fn spawned(&self, pid: Option<u32>) -> Option<(libc::c_int, i32)> {
            let pgid = pid.map_or(0, |p| p as i32);
            self.pgid.store(pgid, SeqCst);
            self.spawning.store(false, SeqCst);
            let sig = self.pending.swap(0, SeqCst);
            (sig != 0).then_some((sig, pgid))
        }

        /// The step is over. Returns a signal a late handler deferred, which
        /// the caller must still act on; its group is not killed, since the
        /// command has been reaped and the id may be reused.
        pub(super) fn end(&self) -> Option<libc::c_int> {
            self.spawning.store(false, SeqCst);
            self.done.store(true, SeqCst);
            self.pgid.store(0, SeqCst);
            let sig = self.pending.swap(0, SeqCst);
            (sig != 0).then_some(sig)
        }
    }

    static STATE: State = State::new();

    /// Kill the group, then stop tak the way `sig` would have. The signal is
    /// blocked while a handler runs, so from there the raise takes effect —
    /// with the default action — as soon as the handler returns.
    fn die(sig: libc::c_int, pgid: i32) {
        // SAFETY: killpg, signal and raise are async-signal-safe.
        unsafe {
            if pgid > 0 {
                libc::killpg(pgid, libc::SIGKILL);
            }
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    extern "C" fn forward(sig: libc::c_int) {
        if let Act::Die { pgid } = STATE.on_signal(sig) {
            die(sig, pgid);
        }
    }

    pub struct Guard {
        previous: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl Guard {
        /// Install the handlers, before spawning: a signal between here and
        /// [`Guard::spawned`] is held until the group is known.
        pub fn install() -> Guard {
            STATE.begin();
            let mut previous = Vec::new();
            for sig in SIGNALS {
                // SAFETY: plain-data structs, filled in before use; the
                // handler is an `extern "C" fn(c_int)`, as sa_sigaction
                // expects without SA_SIGINFO.
                unsafe {
                    let mut new: libc::sigaction = std::mem::zeroed();
                    new.sa_sigaction = forward as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    libc::sigemptyset(&mut new.sa_mask);
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(sig, &new, &mut old) == 0 {
                        previous.push((sig, old));
                    }
                }
            }
            Guard { previous }
        }

        /// The spawn returned: `pid` leads the new group, or `None` if the
        /// spawn failed. A signal that arrived during it is acted on now.
        pub fn spawned(pid: Option<u32>) {
            if let Some((sig, pgid)) = STATE.spawned(pid) {
                die(sig, pgid);
            }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            // Before the previous handlers come back, so a signal one of ours
            // deferred late is still acted on.
            if let Some(sig) = STATE.end() {
                die(sig, 0);
            }
            for (sig, old) in &self.previous {
                // SAFETY: restores the action saved by `install`.
                unsafe {
                    libc::sigaction(*sig, old, std::ptr::null_mut());
                }
            }
        }
    }

    /// Driven on a private `State`, so no real signal is sent and the
    /// handler's own state is untouched: every way a handler and the version
    /// step can interleave ends with the signal acted on.
    #[cfg(test)]
    mod tests {
        use super::{Act, SeqCst, State};

        #[test]
        fn a_signal_during_the_spawn_is_held_until_the_group_is_known() {
            let s = State::new();
            s.begin();
            assert_eq!(s.on_signal(libc::SIGTERM), Act::Defer, "no group yet");
            assert_eq!(s.spawned(Some(42)), Some((libc::SIGTERM, 42)));
            assert_eq!(s.on_signal(libc::SIGINT), Act::Die { pgid: 42 });
            assert_eq!(s.end(), None);

            s.begin();
            assert_eq!(s.spawned(Some(7)), None, "nothing arrived");
            assert_eq!(s.end(), None);

            s.begin();
            assert_eq!(s.on_signal(libc::SIGINT), Act::Defer);
            assert_eq!(
                s.spawned(None),
                Some((libc::SIGINT, 0)),
                "a failed spawn still stops tak"
            );
            assert_eq!(s.end(), None);

            assert_eq!(
                s.on_signal(libc::SIGHUP),
                Act::Die { pgid: 0 },
                "after the step"
            );
            assert!(!s.wants_to_defer(), "and it never defers there");
        }

        /// A handler that decided to defer, then was preempted: whenever it
        /// resumes, its signal is acted on.
        #[test]
        fn a_handler_preempted_past_the_step_still_acts() {
            let s = State::new();

            // Until after the step ended: it sees `done` and stops tak,
            // without killing a group whose id may be reused.
            s.begin();
            assert!(s.wants_to_defer());
            assert_eq!(s.spawned(Some(42)), None);
            assert_eq!(s.end(), None);
            assert_eq!(s.defer(libc::SIGTERM), Act::Die { pgid: 0 });

            // Until after the spawn: it sees the group. Its write is also
            // left for `end`; acting twice on a signal is harmless.
            s.begin();
            assert!(s.wants_to_defer());
            assert_eq!(s.spawned(Some(42)), None);
            assert_eq!(s.defer(libc::SIGINT), Act::Die { pgid: 42 });
            assert_eq!(s.end(), Some(libc::SIGINT));

            // Its write landed before `end` took `pending` (and it read the
            // group before `spawned` stored it): `end` acts on it.
            s.begin();
            assert_eq!(s.spawned(Some(42)), None);
            s.pending.store(libc::SIGHUP, SeqCst);
            assert_eq!(s.end(), Some(libc::SIGHUP), "acted on at teardown");
        }
    }
}

/// Stop a timed-out `version_cmd` and everything it started, then reap it.
///
/// On Unix that is its whole process group, so a helper it spawned cannot go
/// on using CPU through the samples that follow, or hold the output pipes
/// open. Elsewhere only the command itself is stopped.
///
/// Only on a timeout: a command that exits on its own may have started a
/// daemon on purpose — a version check that launches a language server or
/// build daemon — and that is the tool's business, not tak's to kill.
fn stop_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child leads its own group (`process_group(0)`), so its pid is
        // the group id. SAFETY: killpg only sends a signal.
        let pgid = child.id() as libc::pid_t;
        if unsafe { libc::killpg(pgid, libc::SIGKILL) } != 0 {
            let _ = child.kill();
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

fn version_once(argv: &[String], site: &Site, timeout: Duration) -> Result<String> {
    let bin = argv
        .first()
        .map(String::as_str)
        .unwrap_or("(empty command)");
    let mut cmd = command(argv, site)?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so a timeout can stop everything it started:
    // a helper left running would compete with the samples that follow.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    // Being in its own group, it no longer gets the terminal's Ctrl-C; this
    // passes a fatal signal on to it until the guard drops.
    #[cfg(unix)]
    let _forward = forward_signals::Guard::install();
    let spawned = cmd.spawn();
    // Also when the spawn failed, so a signal held during it still stops tak.
    #[cfg(unix)]
    forward_signals::Guard::spawned(spawned.as_ref().ok().map(std::process::Child::id));
    let mut child = spawned.with_context(|| format!("failed to spawn `{bin}`"))?;

    // One thread per stream, so neither pipe can fill while the other is
    // read. They are never joined: a background process the command started
    // may hold a pipe open long after it exits, and the run must not wait on
    // that. Each thread ends when its pipe finally closes.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let spawn_reader = |r: Option<Box<dyn std::io::Read + Send>>| {
        let kept = Arc::new(Mutex::new(Vec::new()));
        if let Some(r) = r {
            let (kept, tx) = (Arc::clone(&kept), done_tx.clone());
            std::thread::spawn(move || {
                keep_prefix(r, VERSION_OUTPUT_CAP, &kept);
                let _ = tx.send(());
            });
        } else {
            let _ = done_tx.send(());
        }
        kept
    };
    let stdout = spawn_reader(child.stdout.take().map(|o| Box::new(o) as _));
    let stderr = spawn_reader(child.stderr.take().map(|e| Box::new(e) as _));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                stop_group(&mut child);
                bail!(
                    "`{bin}` did not finish within {}, so it was stopped",
                    crate::progress::fmt(timeout)
                );
            }
            Err(e) => {
                stop_group(&mut child);
                return Err(e).with_context(|| format!("failed to wait for `{bin}`"));
            }
        }
    };
    let grace = Instant::now() + VERSION_OUTPUT_GRACE;
    for _ in 0..2 {
        let left = grace.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(left).is_err() {
            break;
        }
    }
    let take = |kept: &Mutex<Vec<u8>>| kept.lock().map(|k| k.clone()).unwrap_or_default();
    let (stdout, stderr) = (take(&stdout), take(&stderr));

    // A failing command's output is an error message or a usage dump, not a
    // version, however much of it there is.
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        bail!(
            "`{bin}` exited with {status}: {}",
            stderr
                .lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .unwrap_or("(no output)")
        );
    }
    let first = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    };
    first(&stdout)
        .or_else(|| first(&stderr))
        .with_context(|| format!("`{bin}` printed nothing"))
}

/// `ok` applies to the subject under valgrind: cachegrind exits with its
/// client's code, and re-raises the signal a client died of.
fn count(
    cmd: &[String],
    prepare: Option<&[String]>,
    site: &Site,
    ok: &[i32],
) -> Result<Option<Counted>> {
    if !valgrind_available() {
        return Ok(None);
    }

    let mut samples: Vec<u64> = Vec::with_capacity(COUNTER_RUNS as usize);
    for _ in 0..COUNTER_RUNS {
        if let Some(p) = prepare {
            prepare_once(p, site)?;
        }
        let mut argv: Vec<String> = [
            "valgrind",
            "--tool=cachegrind",
            "--cache-sim=no",
            "--branch-sim=no",
            "--cachegrind-out-file=/dev/null",
        ]
        .map(String::from)
        .to_vec();
        argv.extend_from_slice(cmd);
        let mut c = command(&argv, site)?;
        c.stdout(Stdio::null());
        let out = c.output().context("failed to run valgrind")?;

        // cachegrind writes its summary to stderr as e.g. "I refs:  48,349,132".
        let stderr = String::from_utf8_lossy(&out.stderr);
        let bin = cmd.first().map(String::as_str).unwrap_or("(empty command)");
        accepted(bin, out.status, ok, " under valgrind")?;
        match parse_irefs(&stderr) {
            Some(n) => samples.push(n),
            // Valgrind is installed but produced no summary — a real failure,
            // not the same thing as valgrind being absent. Reporting it as
            // absent sends people off installing something they already have.
            None => bail!(
                "valgrind ran but emitted no `I refs` summary: {}",
                stderr.lines().last().unwrap_or("(no output)").trim()
            ),
        }
    }

    Ok(Some(Counted {
        min: *samples.iter().min().expect("COUNTER_RUNS > 0"),
        max: *samples.iter().max().expect("COUNTER_RUNS > 0"),
        runs: COUNTER_RUNS,
    }))
}

/// Extract the `I refs:` count from cachegrind's stderr summary.
fn parse_irefs(stderr: &str) -> Option<u64> {
    let line = stderr.lines().find(|l| l.contains("I refs:"))?;
    let digits: String = line
        .rsplit(':')
        .next()?
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cachegrind_summary() {
        let s = "==12== I refs:      48,349,132\n";
        assert_eq!(parse_irefs(s), Some(48_349_132));
    }

    #[test]
    fn missing_summary_is_none_not_panic() {
        assert_eq!(parse_irefs("valgrind: command not found"), None);
    }

    /// What `subject` removes, as configured.
    fn removals(settings: &Settings) -> Vec<String> {
        subject("true", settings)
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    /// Construction-level check. The end-to-end proof that a subject cannot see
    /// the token lives in `tests/subject_env.rs`, which runs the real binary.
    #[test]
    fn subject_commands_remove_the_default_denied_variables() {
        let removed = removals(&Settings::default());
        for key in Settings::default().scrubbed_env() {
            assert!(removed.contains(&key.to_string()), "{key} not removed");
        }
    }

    /// The removal follows the setting rather than a compiled-in list, which is
    /// the whole point of routing it through `Settings`.
    #[test]
    fn the_removal_follows_the_settings() {
        let removed = removals(&Settings {
            env_deny: vec!["CUSTOM_SECRET".into()],
            env_allow: Vec::new(),
            ..Settings::default()
        });
        assert!(removed.contains(&"CUSTOM_SECRET".to_string()));
        assert!(!removed.contains(&"GITHUB_TOKEN".to_string()));
    }

    /// An allowed variable is not removed, so a subject that genuinely needs
    /// one can still be measured.
    #[test]
    fn an_allowed_variable_survives() {
        let removed = removals(&Settings {
            env_deny: vec!["GITHUB_TOKEN".into(), "GH_TOKEN".into()],
            env_allow: vec!["GITHUB_TOKEN".into()],
            ..Settings::default()
        });
        assert!(!removed.contains(&"GITHUB_TOKEN".to_string()));
        assert!(removed.contains(&"GH_TOKEN".to_string()));
    }

    #[test]
    fn zero_runs_is_rejected_not_a_panic() {
        let plan = Plan {
            cmd: vec!["true".into()],
            warmup: 0,
            runs: 0,
            dir: None,
            settings: Settings::default(),
        };
        assert!(wall(&plan).is_err());
    }

    #[test]
    fn a_failed_subject_is_not_recorded_as_a_fast_run() {
        #[cfg(unix)]
        let cmd = vec!["/bin/sh".into(), "-c".into(), "exit 42".into()];
        #[cfg(windows)]
        let cmd = vec!["cmd".into(), "/C".into(), "exit /B 42".into()];

        let err = wall(&Plan {
            cmd,
            warmup: 0,
            runs: 1,
            dir: None,
            settings: Settings::default(),
        })
        .unwrap_err();

        assert!(format!("{err:#}").contains("exited with"), "{err:#}");
    }

    #[test]
    fn wall_reports_min_le_p50_le_max() {
        let plan = Plan {
            cmd: vec!["true".into()],
            warmup: 1,
            runs: 5,
            dir: None,
            settings: Settings::default(),
        };
        let m = wall(&plan).unwrap();
        assert!(m["wall_min_ms"] <= m["wall_p50_ms"]);
        assert!(m["wall_p50_ms"] <= m["wall_max_ms"]);
        assert_eq!(m["wall_n"], 5.0);
    }

    /// Every subject gets exactly its warmups and runs, warmups first.
    #[test]
    fn the_schedule_gives_each_subject_its_counts() {
        let counts = [(2, 10), (1, 5), (0, 3)];
        let order = schedule(&counts, 7);
        for (i, &(warmup, runs)) in counts.iter().enumerate() {
            let of = |timed| {
                order
                    .iter()
                    .filter(|s| s.subject == i && s.timed == timed)
                    .count()
            };
            assert_eq!(of(false), warmup as usize, "subject {i} warmups");
            assert_eq!(of(true), runs as usize, "subject {i} runs");
        }
        let first_timed = order.iter().position(|s| s.timed).unwrap();
        assert!(order[first_timed..].iter().all(|s| s.timed));
    }

    #[test]
    fn the_same_seed_gives_the_same_order() {
        let counts = [(1, 8), (1, 8), (1, 8)];
        assert_eq!(schedule(&counts, 42), schedule(&counts, 42));
        assert_ne!(schedule(&counts, 42), schedule(&counts, 43));
    }

    /// Interleaving is the point: with equal counts, every round holds each
    /// subject once, and the order is not the same in every round.
    #[test]
    fn each_round_runs_every_subject_once_in_a_varying_order() {
        let order = schedule(&[(0, 20), (0, 20), (0, 20)], 1);
        let rounds: Vec<Vec<usize>> = order
            .chunks(3)
            .map(|r| r.iter().map(|s| s.subject).collect())
            .collect();
        assert_eq!(rounds.len(), 20);
        for r in &rounds {
            let mut sorted = r.clone();
            sorted.sort();
            assert_eq!(sorted, [0, 1, 2], "round {r:?}");
        }
        assert!(rounds.iter().any(|r| r != &rounds[0]), "never shuffled");
    }

    /// A subject with half the runs takes part in alternate rounds, not only
    /// the first half — otherwise drift late in the run lands on the others.
    #[test]
    fn a_subject_with_fewer_runs_is_spread_across_the_run() {
        let order = schedule(&[(0, 10), (0, 5)], 3);
        let fewer: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|(_, s)| s.subject == 1)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(fewer.len(), 5);
        assert!(
            *fewer.last().unwrap() >= order.len() - 3,
            "last sample of the smaller subject is near the end: {fewer:?} of {}",
            order.len()
        );
    }

    #[test]
    fn a_benchmark_seed_depends_on_its_name_and_the_run_seed() {
        assert_eq!(seed_for(1, "a"), seed_for(1, "a"));
        assert_ne!(seed_for(1, "a"), seed_for(1, "b"));
        assert_ne!(seed_for(1, "a"), seed_for(2, "a"));
    }

    #[test]
    fn stats_report_the_sample_standard_deviation() {
        let m = stats(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert_eq!(m["wall_min_ms"], 2.0);
        assert_eq!(m["wall_max_ms"], 9.0);
        assert_eq!(m["wall_mean_ms"], 5.0);
        // sqrt(32 / 7)
        assert!((m["wall_stddev_ms"] - 2.138_089_935).abs() < 1e-6);
        assert_eq!(stats(&[3.0])["wall_stddev_ms"], 0.0);
    }

    /// A failing subject is dropped; the others still get every sample.
    #[cfg(unix)]
    #[test]
    fn a_failing_subject_does_not_stop_the_others() {
        let mk = |name: &str, cmd: &[&str]| Subject {
            name: name.into(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            prepare: None,
            setup: None,
            setup_dir: None,
            check: None,
            dir: None,
            version_cmd: None,
            env: BTreeMap::new(),
            vars: BTreeMap::new(),
            when: None,
            runs: Runs::Fixed(4),
            auto: AutoRuns {
                budget: Duration::from_secs(30),
                min: 5,
                max: 50,
            },
            warmup: 1,
            counters: false,
            ok_exit_codes: vec![0],
        };
        let res = interleaved(
            &[mk("ok", &["true"]), mk("bad", &["false"])],
            5,
            &Settings::default(),
            &mut Quiet,
        );
        assert_eq!(res[0].as_ref().unwrap().times.len(), 4);
        assert!(format!("{:#}", res[1].as_ref().unwrap_err()).contains("exited with"));
    }

    /// Runs `sh -c script` as a `version_cmd` with the given deadline.
    #[cfg(unix)]
    fn version_sh(script: &str, env: &[(&str, &str)], timeout: Duration) -> Result<String> {
        let settings = Settings::default();
        let env: BTreeMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let site = Site {
            dir: None,
            env: &env,
            settings: &settings,
        };
        version_once(&["sh".into(), "-c".into(), script.into()], &site, timeout)
    }

    /// The first non-empty line, from stdout or else stderr; a failure is an
    /// error naming the command, which the caller turns into a warning.
    #[cfg(unix)]
    #[test]
    fn a_version_is_the_first_line_printed() {
        let sh = |script: &str| version_sh(script, &[("V", "9.9")], VERSION_TIMEOUT);
        assert_eq!(
            sh("printf '\\n  tool %s  \\nbuilt today\\n' \"$V\"").unwrap(),
            "tool 9.9",
            "first non-empty line, trimmed, in the subject's env"
        );
        assert_eq!(sh("echo 'java 21' >&2").unwrap(), "java 21");
        let err = format!("{:#}", sh("echo nope >&2; exit 2").unwrap_err());
        assert!(err.contains("`sh`") && err.contains("nope"), "{err}");
        assert!(sh("true").is_err(), "no output is no version");
    }

    /// Output past the cap is read and dropped, so a command that prints a
    /// lot still finishes, and its first line is what counts.
    #[cfg(unix)]
    #[test]
    fn a_version_is_read_from_a_capped_prefix() {
        let sh = |script: &str| version_sh(script, &[], VERSION_TIMEOUT);
        assert_eq!(sh("yes 'tool 1.0' | head -c 10000000").unwrap(), "tool 1.0");
        assert_eq!(
            sh("yes 'java 21' | head -c 10000000 >&2").unwrap(),
            "java 21",
            "a flood on stderr"
        );
        let kept = Mutex::new(Vec::new());
        let mut src = std::io::Cursor::new(vec![7u8; 100]);
        keep_prefix(&mut src, 10, &kept);
        assert_eq!(kept.lock().unwrap().len(), 10, "only the cap is kept");
        assert_eq!(
            src.position(),
            100,
            "the rest is read, not left in the pipe"
        );
    }

    /// A failing command that prints a lot — a usage dump after an unknown
    /// flag — is a failure, not a version, however long its output.
    #[cfg(unix)]
    #[test]
    fn a_failing_version_cmd_is_an_error_even_past_the_cap() {
        let err = version_sh(
            "yes 'usage: tool [flags]' | head -c 100000; echo 'unknown flag' >&2; exit 1",
            &[],
            VERSION_TIMEOUT,
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(
            err.contains("exited with") && err.contains("unknown flag"),
            "{err}"
        );
    }

    /// One that never finishes, even one that only floods stderr, is stopped
    /// at the deadline and says so.
    #[cfg(unix)]
    #[test]
    fn a_version_cmd_that_never_finishes_is_stopped() {
        let t = Duration::from_millis(300);
        let began = Instant::now();
        // `exec`, so the process stopped at the deadline is the flood itself.
        for script in ["exec yes 'tool 2.0' >&2", "exec sleep 30"] {
            let err = format!("{:#}", version_sh(script, &[], t).unwrap_err());
            assert!(err.contains("did not finish"), "{script}: {err}");
        }
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
    }

    /// A timeout stops what the command started too, not just the command:
    /// a helper left running would compete with the samples that follow.
    #[cfg(unix)]
    #[test]
    fn a_timeout_stops_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pidfile.display());
        let err = version_sh(&script, &[], Duration::from_millis(300)).unwrap_err();
        assert!(format!("{err:#}").contains("did not finish"), "{err:#}");
        let pid = std::fs::read_to_string(&pidfile).unwrap();
        let pid = pid.trim();
        let until = Instant::now() + Duration::from_secs(2);
        while running(pid) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !running(pid),
            "the background sleep {pid} outlived the timeout"
        );
    }

    /// Whether `pid` is still running. A zombie is not: once the shell that
    /// started the process is killed with it, only PID 1 can reap it, and in
    /// a container whose PID 1 does not reap orphans it stays a zombie —
    /// which `kill -0` still reports as present.
    #[cfg(unix)]
    fn running(pid: &str) -> bool {
        #[cfg(target_os = "linux")]
        {
            // The state follows the `)` closing the command name, which may
            // itself contain spaces or parentheses, so look for the last one.
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                return false;
            };
            let state = stat
                .rfind(')')
                .and_then(|i| stat[i + 1..].trim_start().chars().next());
            !matches!(state, None | Some('Z' | 'X' | 'x'))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let Ok(out) = Command::new("ps")
                .args(["-o", "stat=", "-p", pid])
                .stderr(Stdio::null())
                .output()
            else {
                return false;
            };
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            out.status.success() && !stat.is_empty() && !stat.starts_with('Z')
        }
    }

    /// A background process that inherits the pipes keeps them open after
    /// the command exits; the version it printed still comes back promptly.
    #[cfg(unix)]
    #[test]
    fn a_background_process_holding_the_pipe_does_not_hold_up_the_version() {
        let began = Instant::now();
        let v = version_sh("sleep 5 & echo 'tool 3.0'", &[], VERSION_TIMEOUT).unwrap();
        assert_eq!(v, "tool 3.0");
        assert!(
            began.elapsed() < Duration::from_secs(3),
            "{:?}",
            began.elapsed()
        );
    }

    /// A prepare step that fails stops its subject, and says it was prepare.
    #[cfg(unix)]
    #[test]
    fn a_failing_prepare_is_reported_as_prepare() {
        let res = interleaved(
            &[Subject {
                name: "x".into(),
                cmd: vec!["true".into()],
                prepare: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo nope >&2; exit 3".into(),
                ]),
                setup: None,
                setup_dir: None,
                check: None,
                dir: None,
                version_cmd: None,
                env: BTreeMap::new(),
                vars: BTreeMap::new(),
                when: None,
                runs: Runs::Fixed(1),
                auto: AutoRuns {
                    budget: Duration::from_secs(30),
                    min: 5,
                    max: 50,
                },
                warmup: 0,
                counters: false,
                ok_exit_codes: vec![0],
            }],
            0,
            &Settings::default(),
            &mut Quiet,
        );
        let msg = format!("{:#}", res[0].as_ref().unwrap_err());
        assert!(msg.contains("prepare") && msg.contains("nope"), "{msg}");
    }

    /// A subject whose command bumps a counter file and whose check fails on
    /// every even-numbered run, warmups included in the count.
    #[cfg(unix)]
    fn alternating(dir: &Path, warmup: u32, runs: u32) -> Subject {
        Subject {
            name: "alt".into(),
            cmd: vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo x >> n".into(),
            ],
            prepare: None,
            setup: None,
            setup_dir: None,
            check: Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                // Arithmetic strips the padding macOS `wc -l` puts before the count.
                "echo check >> checked; runs=$(( $(wc -l < n) )); test $(( runs % 2 )) = 1 || { echo \"run $runs is even\" >&2; exit 1; }".into(),
            ]),
            version_cmd: None,
            dir: Some(dir.to_path_buf()),
            env: BTreeMap::new(),
            vars: BTreeMap::new(),
            when: None,
            runs: Runs::Fixed(runs),
            auto: AutoRuns {
                budget: Duration::from_secs(30),
                min: 5,
                max: 50,
            },
            warmup,
            counters: false,
            ok_exit_codes: vec![0],
        }
    }

    /// A failing check marks its sample and the subject keeps every one;
    /// warmups are never checked.
    #[cfg(unix)]
    #[test]
    fn a_failing_check_marks_the_sample_without_dropping_the_subject() {
        let dir = tempfile::tempdir().unwrap();
        let res = interleaved(
            &[alternating(dir.path(), 1, 6)],
            0,
            &Settings::default(),
            &mut Quiet,
        );
        let s = res[0].as_ref().unwrap();
        assert_eq!(s.times.len(), 6);
        // Run 1 is the warmup; timed runs 2..=7 fail on the even ones.
        assert_eq!(s.checks, [false, true, false, true, false, true]);
        assert_eq!(s.passed(), 3);
        assert!(
            s.first_failure
                .as_deref()
                .unwrap()
                .contains("run 2 is even"),
            "{s:?}"
        );
        let checked = std::fs::read_to_string(dir.path().join("checked")).unwrap();
        assert_eq!(checked.lines().count(), 6, "no check after the warmup");
    }

    /// `ok_exit_codes` belong to the command, not its check: a command that
    /// exits 1 under `[0, 1]` is kept with its code, and a check that exits 1
    /// is still a failed check.
    #[cfg(unix)]
    #[test]
    fn ok_exit_codes_do_not_pass_a_check_that_exits_1() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = alternating(dir.path(), 0, 2);
        s.cmd = vec!["/bin/sh".into(), "-c".into(), "exit 1".into()];
        s.check = Some(vec!["/bin/sh".into(), "-c".into(), "exit 1".into()]);
        s.ok_exit_codes = vec![0, 1];
        let res = interleaved(&[s], 0, &Settings::default(), &mut Quiet);
        let s = res[0].as_ref().unwrap();
        assert_eq!(s.exit_codes, [1, 1]);
        assert_eq!(s.checks, [false, false]);
        assert_eq!(s.passed(), 0);
    }

    /// A check that cannot be started is a configuration mistake, not six
    /// failed samples.
    #[cfg(unix)]
    #[test]
    fn a_check_that_cannot_start_drops_the_subject() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = alternating(dir.path(), 0, 2);
        s.check = Some(vec!["/nonexistent/tak-check".into()]);
        let res = interleaved(&[s], 0, &Settings::default(), &mut Quiet);
        let msg = format!("{:#}", res[0].as_ref().unwrap_err());
        assert!(msg.contains("failed to spawn check"), "{msg}");
    }

    /// An observer that records what it was told.
    #[derive(Default)]
    struct Log {
        plans: Vec<Vec<u64>>,
        finished: Vec<usize>,
        /// Every event in order, as `setup:N`, `start:N` and `drop:N`.
        events: Vec<String>,
    }
    impl Observer for Log {
        fn planned(&mut self, remaining: &[u64]) {
            self.plans.push(remaining.to_vec());
        }
        fn setting_up(&mut self, subject: usize) {
            self.events.push(format!("setup:{subject}"));
        }
        fn started(&mut self, subject: usize) {
            self.events.push(format!("start:{subject}"));
        }
        fn dropped(&mut self, subject: usize) {
            self.events.push(format!("drop:{subject}"));
        }
        fn finished(&mut self, subject: usize, _: Duration) {
            self.finished.push(subject);
        }
    }

    /// Every setup runs once, before any sample; a failing one drops its
    /// subject before it takes a sample, and says it was setup.
    #[cfg(unix)]
    #[test]
    fn setups_run_once_first_and_a_failing_one_drops_its_subject() {
        let sh = |script: &str| Some(["/bin/sh", "-c", script].map(String::from).to_vec());
        let mk = |name: &str, setup: Option<Vec<String>>| Subject {
            name: name.into(),
            cmd: vec!["true".into()],
            prepare: None,
            setup,
            setup_dir: None,
            check: None,
            version_cmd: None,
            dir: None,
            env: BTreeMap::new(),
            vars: BTreeMap::new(),
            when: None,
            runs: Runs::Auto,
            auto: AutoRuns {
                budget: Duration::from_secs(30),
                min: 2,
                max: 2,
            },
            warmup: 1,
            counters: false,
            ok_exit_codes: vec![0],
        };
        let mut log = Log::default();
        let res = interleaved(
            &[
                mk("ok", sh("true")),
                mk("bad", sh("echo no fixture >&2; exit 2")),
                mk("none", None),
            ],
            3,
            &Settings::default(),
            &mut log,
        );
        assert_eq!(log.events[..3], ["setup:0", "setup:1", "drop:1"]);
        assert!(!log.events[3..].iter().any(|e| e.starts_with("setup:")));
        assert!(!log.events.contains(&"start:1".to_string()));
        assert_eq!(res[0].as_ref().unwrap().times.len(), 2);
        assert_eq!(res[2].as_ref().unwrap().times.len(), 2);
        let msg = format!("{:#}", res[1].as_ref().unwrap_err());
        assert!(msg.contains("setup") && msg.contains("no fixture"), "{msg}");
        // Setup is not a sample: only warmups and runs finish.
        assert_eq!(log.finished.len(), 2 * (1 + 2));
    }

    /// `runs = "auto"` sizes each subject from its own speed: a subject too
    /// slow for the budget gets the floor, a faster one gets more. Uses real
    /// sleeps, so the fast count is only bounded, not exact.
    #[cfg(unix)]
    #[test]
    fn auto_runs_give_a_slow_subject_the_floor_and_a_fast_one_more() {
        let mk = |name: &str, secs: &str, warmup: u32| Subject {
            name: name.into(),
            cmd: vec!["sleep".into(), secs.into()],
            prepare: None,
            setup: None,
            setup_dir: None,
            check: None,
            dir: None,
            version_cmd: None,
            env: BTreeMap::new(),
            vars: BTreeMap::new(),
            when: None,
            runs: Runs::Auto,
            auto: AutoRuns {
                budget: Duration::from_millis(400),
                min: 2,
                max: 6,
            },
            warmup,
            counters: false,
            ok_exit_codes: vec![0],
        };
        let mut log = Log::default();
        // The slow one has no warmup, so its first kept sample sizes it.
        let res = interleaved(
            &[mk("fast", "0.02", 1), mk("slow", "0.3", 0)],
            9,
            &Settings::default(),
            &mut log,
        );
        let fast = res[0].as_ref().unwrap().times.len();
        let slow = res[1].as_ref().unwrap().times.len();
        assert_eq!(slow, 2, "0.4s budget / 0.3s sample is below the floor");
        assert!(fast > slow && fast <= 6, "fast got {fast}");
        // Planned on the floors first, then on the settled counts.
        assert_eq!(log.plans[0], [1 + 2, 2]);
        assert_eq!(log.plans.len(), 2);
        assert_eq!(log.finished.len(), 1 + fast + slow);
    }

    #[test]
    fn steady_samples_raise_no_warning() {
        assert!(warnings(&[10.0, 10.2, 9.9, 10.1, 10.0, 10.3]).is_empty());
        assert!(warnings(&[10.0, 50.0]).is_empty(), "too few to judge");
    }

    #[test]
    fn a_spike_is_an_outlier() {
        let w = warnings(&[10.0, 10.2, 9.9, 10.1, 40.0, 10.0, 10.1]);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].starts_with("1 of 7 samples are slow outliers"),
            "{w:?}"
        );
    }

    #[test]
    fn a_slow_first_sample_is_called_out() {
        let w = warnings(&[35.0, 10.0, 10.2, 9.9, 10.1, 10.0]);
        assert!(
            w.iter().any(|m| m.contains("first sample took 3.5x")),
            "{w:?}"
        );
    }

    /// With most samples identical the median deviation is zero; a spike
    /// must still be reported.
    #[test]
    fn a_spike_among_identical_samples_is_still_an_outlier() {
        let w = warnings(&[10.0, 10.0, 10.0, 100.0, 10.0]);
        assert!(w.iter().any(|m| m.contains("slow outliers")), "{w:?}");
        assert!(
            warnings(&[10.0; 6]).is_empty(),
            "identical samples are fine"
        );
    }

    /// A fast outlier is the minimum, so it is reported as such rather than
    /// reassured away.
    #[test]
    fn a_fast_outlier_is_flagged_as_possibly_the_minimum() {
        let w = warnings(&[100.0, 101.0, 99.0, 102.0, 10.0, 100.0]);
        assert!(
            w.iter()
                .any(|m| m.contains("fast outliers") && m.contains("minimum")),
            "{w:?}"
        );
    }

    /// Several identical spikes over a flat majority are all caught, not
    /// hidden by a spread they themselves inflate.
    #[test]
    fn several_spikes_over_a_flat_majority_are_caught() {
        let w = warnings(&[10.0, 10.0, 10.0, 10.0, 40.0, 45.0]);
        assert!(
            w.iter()
                .any(|m| m.starts_with("2 of 6 samples are slow outliers")),
            "{w:?}"
        );
        assert!(
            warnings(&[10.0, 10.0, 10.0, 11.0, 10.0]).is_empty(),
            "10% is not a spike"
        );
    }
}
