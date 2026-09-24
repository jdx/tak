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

use crate::config::{AutoRuns, Runs, SELF_TOOL, Subject};
use crate::settings::Settings;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
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

/// Run once, discarding output, returning elapsed wall time in milliseconds.
///
/// No shell. Spawning a shell adds its own startup cost and variance to every
/// sample, which for commands in the 10ms range is a large fraction of the
/// measurement — the same reasoning behind poop's refusal to support one.
fn time_once(cmd: &[String], site: &Site) -> Result<f64> {
    let mut c = command(cmd, site)?;
    c.stdout(Stdio::null()).stderr(Stdio::null());
    let bin = &cmd[0];
    let start = Instant::now();
    let status = c
        .status()
        .with_context(|| format!("failed to spawn `{bin}`"))?;
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    if !status.success() {
        bail!("benchmark subject `{bin}` exited with {status}");
    }
    Ok(elapsed)
}

/// How much of a failing prepare step's stderr to keep for the error message.
/// Enough for the last few lines; a verbose reset command's full output would
/// otherwise sit in memory before every sample.
const PREPARE_STDERR_TAIL: usize = 4096;

/// Run a subject's prepare step. Untimed, so reading its stderr for the error
/// message costs the measurement nothing.
fn prepare_once(cmd: &[String], site: &Site) -> Result<()> {
    use std::io::Read;

    let mut c = command(cmd, site)?;
    let bin = &cmd[0];
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn prepare `{bin}`"))?;
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
            if tail.len() > PREPARE_STDERR_TAIL {
                tail.drain(..tail.len() - PREPARE_STDERR_TAIL);
            }
        }
    }
    let status = child
        .wait()
        .with_context(|| format!("failed to wait for prepare `{bin}`"))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&tail);
        bail!(
            "prepare `{bin}` exited with {status}: {}",
            stderr.lines().last().unwrap_or("(no output)").trim()
        );
    }
    Ok(())
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

/// Measure several subjects against each other, interleaved.
///
/// Returns each subject's timed samples in the order they were taken, or the
/// error that stopped it. A failing subject is dropped from the remaining
/// rounds and the others carry on: one competitor breaking should not throw
/// away a long run's worth of everyone else's samples. Subject directories
/// are used as given, so the caller resolves them first.
///
/// Runs in two phases. The warmups come first, plus one kept sample of every
/// `runs = "auto"` subject that has no warmups, so each auto subject has been
/// timed at least once. Its run count is then fixed from the fastest of those
/// samples, and every remaining timed sample is taken in shuffled rounds.
pub fn interleaved(
    subjects: &[Subject],
    seed: u64,
    settings: &Settings,
    observer: &mut dyn Observer,
) -> Vec<Result<Vec<f64>>> {
    let mut rng = fastrand::Rng::with_seed(seed);
    let mut results: Vec<Result<Vec<f64>>> = subjects
        .iter()
        .map(|s| {
            // Zero runs would leave nothing to report on.
            if s.runs == Runs::Fixed(0) {
                bail!("runs must be at least 1");
            }
            Ok(Vec::new())
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
                u64::from(n).saturating_sub(taken.len() as u64)
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
    results
}

/// An auto subject's run count. One that was never timed — its every sample
/// failed, so it is being dropped anyway — gets the floor.
fn auto_runs(auto: &AutoRuns, fastest: Option<Duration>) -> u32 {
    fastest.map_or(auto.min, |d| auto.runs_for(d))
}

/// Take the given samples, recording timed ones and the fastest slot seen.
fn run_slots(
    slots: &[Slot],
    subjects: &[Subject],
    settings: &Settings,
    results: &mut [Result<Vec<f64>>],
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
            .and_then(|()| time_once(&s.cmd, &site));
        let elapsed = began.elapsed();
        match taken {
            Ok(ms) => {
                if slot.timed {
                    samples.push(ms);
                }
                let f = &mut fastest[slot.subject];
                *f = Some(f.map_or(elapsed, |f| f.min(elapsed)));
                observer.finished(slot.subject, elapsed);
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
/// Nothing is dropped or corrected — the minimum is already robust to both —
/// the point is to say when a comparison deserves a second run.
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
    if mad > 0.0 {
        let outliers = samples
            .iter()
            .filter(|&&x| 0.6745 * (x - med).abs() / mad > 3.5)
            .count();
        if outliers > 0 {
            out.push(format!(
                "{outliers} of {n} samples are outliers; something else was running, or the command's \
                 work varies. The minimum is unaffected, but consider a quieter machine or more runs."
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
        dir: plan.dir.clone(),
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
    Ok(stats(&samples))
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
    )
}

/// [`instructions`] for a declared subject: its environment, and its prepare
/// step before every cachegrind run, since each run has to start from the
/// same state the timed samples did.
pub fn subject_instructions(s: &Subject, settings: &Settings) -> Result<Option<Counted>> {
    count(
        &s.cmd,
        s.prepare.as_deref(),
        &Site {
            dir: s.dir.as_deref(),
            env: &s.env,
            settings,
        },
    )
}

fn count(cmd: &[String], prepare: Option<&[String]>, site: &Site) -> Result<Option<Counted>> {
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
        if !out.status.success() {
            let bin = cmd.first().map(String::as_str).unwrap_or("(empty command)");
            bail!(
                "benchmark subject `{bin}` exited with {} under valgrind",
                out.status
            );
        }
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
            dir: None,
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
        };
        let res = interleaved(
            &[mk("ok", &["true"]), mk("bad", &["false"])],
            5,
            &Settings::default(),
            &mut Quiet,
        );
        assert_eq!(res[0].as_ref().unwrap().len(), 4);
        assert!(format!("{:#}", res[1].as_ref().unwrap_err()).contains("exited with"));
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
                dir: None,
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
            }],
            0,
            &Settings::default(),
            &mut Quiet,
        );
        let msg = format!("{:#}", res[0].as_ref().unwrap_err());
        assert!(msg.contains("prepare") && msg.contains("nope"), "{msg}");
    }

    /// An observer that records what it was told.
    #[derive(Default)]
    struct Log {
        plans: Vec<Vec<u64>>,
        finished: Vec<usize>,
    }
    impl Observer for Log {
        fn planned(&mut self, remaining: &[u64]) {
            self.plans.push(remaining.to_vec());
        }
        fn finished(&mut self, subject: usize, _: Duration) {
            self.finished.push(subject);
        }
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
            dir: None,
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
        };
        let mut log = Log::default();
        // The slow one has no warmup, so its first kept sample sizes it.
        let res = interleaved(
            &[mk("fast", "0.02", 1), mk("slow", "0.3", 0)],
            9,
            &Settings::default(),
            &mut log,
        );
        let fast = res[0].as_ref().unwrap().len();
        let slow = res[1].as_ref().unwrap().len();
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
        assert!(w[0].starts_with("1 of 7 samples are outliers"), "{w:?}");
    }

    #[test]
    fn a_slow_first_sample_is_called_out() {
        let w = warnings(&[35.0, 10.0, 10.2, 9.9, 10.1, 10.0]);
        assert!(
            w.iter().any(|m| m.contains("first sample took 3.5x")),
            "{w:?}"
        );
    }
}
