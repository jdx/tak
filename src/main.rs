//! tak — benchmark command-line programs and track their performance over time.
//!
//! Pre-v1: interfaces and behavior are not finalized and may change between releases.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::Path;
use usage_rs::{Args, Cli, Subcommands};

use tak_cli::backfill;
use tak_cli::compare;
use tak_cli::config::{
    self, AutoRuns, Config, DEFAULT_BUDGET, DEFAULT_MAX_RUNS, DEFAULT_MIN_RUNS, DEFAULT_RUNS,
    DEFAULT_WARMUP, Runs, SELF_TOOL, Subject,
};
use tak_cli::export::{self, ExportResult};
use tak_cli::measure::{self, Plan};
use tak_cli::notes;
use tak_cli::record::{Record, SCHEMA_VERSION};
use tak_cli::settings::{CliLayer, EnvLayer, Settings, TakConfigLayer, config_source};

#[derive(Cli)]
#[usage(completion = true)]
#[usage(
    name = "tak",
    bin = "tak",
    version,
    about = "CLI performance, tracked",
    unknown_flags = "error",
    config = tak_cli::settings::Settings
)]
struct Cli {
    #[usage(subcommand)]
    cmd: Cmd,

    // Global so a setting is spelled the same wherever it applies, rather than
    // being repeated per subcommand and drifting. `setting = "..."` is the
    // executable binding: what these flags are given lands in the settings
    // layer the parser hands back, and a test holds the bindings against the
    // registry so neither can drift from the other.
    /// Remove a variable from the environment of measured commands. Repeatable.
    /// Replaces the default list rather than adding to it.
    #[usage(long, global, value_name = "VAR", setting = "env_deny")]
    env_deny: Vec<String>,
    /// Keep a variable that --env-deny would remove. Repeatable.
    #[usage(long, global, value_name = "VAR", setting = "env_allow")]
    env_allow: Vec<String>,
    /// Percentage an instruction count may rise before `compare` fails.
    #[usage(long, global, value_name = "PCT", setting = "gate_pct")]
    gate_pct: Option<f64>,
    /// Leave the line naming tak off the end of generated reports.
    // `SetFalse`: the long spelling is the negation of the setting, so `--no-credit`
    // contributes `false` to the settings layer and its absence contributes nothing.
    #[usage(long = "no-credit", action = usage_rs::ArgAction::SetFalse, global = true)]
    #[usage(default = "true", setting = "credit")]
    credit: bool,
    /// Machine class to record under. Overrides the derived name.
    #[usage(long, global, value_name = "CLASS", setting = "runner_class")]
    runner: Option<String>,
}

#[derive(Subcommands)]
enum Cmd {
    /// Generate a self-contained shell completion script
    Completion {
        /// Shell to generate completions for
        #[usage(
            arg,
            choices("bash", "elvish", "zsh", "fish", "nu", "nushell", "powershell", "pwsh")
        )]
        shell: String,
    },
    /// Benchmark a command, or everything declared in tak.toml.
    Run {
        /// Name to record this measurement under. With no command, selects a
        /// single benchmark from tak.toml instead of running all of them.
        #[usage(long)]
        bench: Option<String>,
        /// Timed runs, or `auto` to size each subject from how long its
        /// samples take. Overrides tak.toml when both are given.
        #[usage(long, value_name = "N|auto")]
        runs: Option<String>,
        /// Untimed warmup runs. Overrides tak.toml when both are given.
        #[usage(long)]
        warmup: Option<u32>,
        /// Skip instruction counting even where valgrind is available.
        #[usage(long)]
        no_counters: bool,
        /// Append the result to refs/notes/tak for the current commit.
        #[usage(long)]
        record: bool,
        /// Do not report progress on stderr.
        #[usage(long)]
        no_progress: bool,
        /// Measure only this subject of each multi-subject benchmark.
        /// Repeatable. Benchmarks with none of the named subjects are skipped.
        #[usage(long, value_name = "NAME")]
        subject: Vec<String>,
        /// Seed for the order subjects are sampled in. Every multi-subject
        /// run prints the seed it used, so an order can be repeated.
        #[usage(long, value_name = "N")]
        seed: Option<u64>,
        /// Read this file instead of searching for tak.toml.
        #[usage(long, value_name = "PATH")]
        config: Option<std::path::PathBuf>,
        /// Print what would run — every subject's command, prepare, directory,
        /// environment and run count, templates rendered — without running it.
        #[usage(long)]
        dry_run: bool,
        /// Write every sample and summary to PATH as hyperfine-compatible JSON.
        #[usage(long, value_name = "PATH")]
        export_json: Option<std::path::PathBuf>,
        /// Command to benchmark, after `--`. Omit to run what tak.toml declares.
        #[usage(arg, double_dash = "required")]
        cmd: Vec<String>,
    },
    /// Show recorded history for a commit.
    History {
        /// Revision to read. Defaults to HEAD.
        #[usage(arg, default = "HEAD")]
        rev: String,
        /// Remote to refresh notes from.
        #[usage(long, default = "origin")]
        remote: String,
    },
    /// Push recorded measurements to the remote.
    Push(RemoteArgs),
    /// Move measurements between a read-only job and a trusted publisher.
    Artifact(Box<ArtifactArgs>),
    /// Teach plain `git fetch` about the notes ref.
    Init(RemoteArgs),
    /// Benchmark published release binaries to bootstrap history.
    ///
    /// A new adopter's first chart is empty. Rather than rebuilding a project at
    /// a hundred historical commits, download what it already published.
    Backfill {
        /// Repository to pull releases from, as "owner/name". Defaults to the
        /// `origin` remote of the current repository.
        #[usage(long)]
        repo: Option<String>,
        /// Executable name to look for inside each release archive. Defaults to
        /// the repository name.
        #[usage(long)]
        bin: Option<String>,
        /// Arguments passed to the downloaded binary, after `--`.
        /// Defaults to `--version`, which every CLI answers cheaply.
        #[usage(arg, double_dash = "required")]
        args: Vec<String>,
        /// Name to record measurements under.
        #[usage(long, default = "release")]
        bench: String,
        /// Most recent releases to measure.
        #[usage(long, default = "20")]
        limit: usize,
        /// Timed runs per release.
        #[usage(long, default = "10")]
        runs: u32,
        /// Measure but do not write to refs/notes/tak.
        #[usage(long)]
        dry_run: bool,
    },
    /// Compare this commit's measurements against another's.
    ///
    /// Fails when an instruction count has risen by more than `gate_pct`. Wall
    /// clock is reported and never gated.
    Compare {
        /// Revision to compare against.
        #[usage(arg, default = "origin/main")]
        base: String,
        /// Revision to compare. Defaults to HEAD.
        #[usage(long, default = "HEAD")]
        rev: String,
        /// Remote to refresh notes from.
        #[usage(long, default = "origin")]
        remote: String,
        /// Report without failing, whatever the numbers say.
        #[usage(long)]
        no_gate: bool,
    },
    /// Diagnose the git-notes plumbing.
    Doctor,
    /// Show every setting, its resolved value, and where that value came from.
    Settings {
        /// Include the full description of each setting.
        #[usage(long)]
        docs: bool,
    },
    /// Generate the CLI specification used to build the documentation.
    #[usage(hide)]
    Usage,
}

#[derive(Args)]
struct ArtifactArgs {
    #[usage(subcommand)]
    cmd: ArtifactCmd,
}

#[derive(Subcommands)]
enum ArtifactCmd {
    /// Export locally recorded measurements as one portable file.
    Export {
        /// Artifact file to write.
        #[usage(long, value_name = "PATH")]
        output: std::path::PathBuf,
        /// Revision whose local measurements should be exported.
        #[usage(long, default = "HEAD")]
        rev: String,
    },
    /// Validate, import, and publish a measurement artifact.
    Publish {
        /// Artifact file produced by `tak artifact export`.
        #[usage(arg)]
        path: std::path::PathBuf,
        /// Trusted revision the artifact must target.
        #[usage(long, value_name = "REV")]
        expect: String,
        /// Remote to synchronize with.
        #[usage(long, default = "origin")]
        remote: String,
    },
}

#[derive(Args)]
struct RemoteArgs {
    /// Remote to use for git-notes synchronization.
    #[usage(long, default = "origin")]
    remote: String,
}

/// Identify the machine class. Series must be partitioned on this — moving
/// between runner types shifts absolute numbers enough to look like a regression,
/// which is a documented failure mode of every threshold-based CI benchmark.
///
/// An explicit `runner_class` wins. It is how a project partitions on something
/// the derived name cannot see: a compiler or image upgrade changes the numbers
/// without changing the machine, and tak has no way to detect that on its own.
fn runner_class(settings: &Settings) -> String {
    if !settings.runner_class.trim().is_empty() {
        return settings.runner_class.clone();
    }
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        let os = std::env::var("RUNNER_OS").unwrap_or_else(|_| "unknown".into());
        let arch = std::env::var("RUNNER_ARCH").unwrap_or_else(|_| "unknown".into());
        return format!("gha-{}-{}", os.to_lowercase(), arch.to_lowercase());
    }
    format!("local-{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// RFC 3339 to second resolution, without pulling in a date crate for a skeleton.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm), epoch shifted to 0000-03-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// `tak run`'s options, gathered so they travel as one value.
struct RunOpts {
    bench: Option<String>,
    runs: Option<Runs>,
    warmup: Option<u32>,
    no_counters: bool,
    record: bool,
    no_progress: bool,
    subjects: Vec<String>,
    seed: Option<u64>,
    export_json: Option<std::path::PathBuf>,
    config: Option<std::path::PathBuf>,
    dry_run: bool,
}

/// One subject's successful measurement.
struct Measured {
    bench: String,
    subject: Subject,
    samples: Vec<f64>,
    record: Record,
}

fn cmd_run(opts: RunOpts, cmd: Vec<String>, settings: &Settings) -> Result<()> {
    // An explicit command always wins; tak.toml is only consulted when none is
    // given, so ad-hoc measurement never depends on repository state.
    if cmd.is_empty() {
        return run_declared(opts, settings);
    }
    if !opts.subjects.is_empty() {
        bail!(
            "--subject selects subjects declared in tak.toml; it cannot be used with a command after `--`"
        );
    }
    let bench = opts.bench.clone().unwrap_or_else(|| "default".to_string());
    let subject = Subject {
        name: SELF_TOOL.to_string(),
        cmd,
        prepare: None,
        dir: None,
        env: BTreeMap::new(),
        vars: BTreeMap::new(),
        when: None,
        runs: opts.runs.unwrap_or(Runs::Fixed(DEFAULT_RUNS)),
        auto: AutoRuns {
            budget: DEFAULT_BUDGET,
            min: DEFAULT_MIN_RUNS,
            max: DEFAULT_MAX_RUNS,
        },
        warmup: opts.warmup.unwrap_or(DEFAULT_WARMUP),
        counters: true,
    };
    let seed = opts.seed.unwrap_or_else(random_seed);
    if opts.dry_run {
        print_plan(
            &bench,
            false,
            std::slice::from_ref(&subject),
            opts.no_counters,
        );
        return Ok(());
    }
    let (measured, _) = measure_bench(&bench, &[subject], false, seed, &opts, settings)?;
    finish(measured, Vec::new(), &opts, seed, settings)
}

/// Run the benchmarks declared in `tak.toml`.
fn run_declared(opts: RunOpts, settings: &Settings) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let found = match &opts.config {
        // Absolute, so the directory commands are anchored to is too: a
        // relative one would be re-resolved from each subject's own `dir`,
        // and a bare `tak.toml` would have no parent at all.
        Some(path) => {
            let path = std::path::absolute(path)
                .with_context(|| format!("could not resolve {}", path.display()))?;
            let cfg = Config::load(&path)?;
            Some((path, cfg))
        }
        None => Config::find(&cwd)?,
    };
    let Some((path, cfg)) = found else {
        bail!(
            "no command given and no {} found in {} or any parent.\n\
             Pass a command after `--`, or declare one:\n\n\
             \x20   [bench.startup]\n\
             \x20   cmd = [\"./mycli\", \"--version\"]",
            config::FILE_NAME,
            cwd.display()
        );
    };

    let selected: Vec<_> = match &opts.bench {
        Some(name) => {
            let b = cfg.bench.get(name).with_context(|| {
                format!(
                    "no benchmark `{name}` in {} (found: {})",
                    path.display(),
                    if cfg.bench.is_empty() {
                        "none".to_string()
                    } else {
                        cfg.bench.keys().cloned().collect::<Vec<_>>().join(", ")
                    }
                )
            })?;
            vec![(name.clone(), b)]
        }
        None => cfg.bench.iter().map(|(k, v)| (k.clone(), v)).collect(),
    };

    // Commands are relative to tak.toml, not to wherever this was invoked.
    let root = path.parent().map(Path::to_path_buf).unwrap_or_default();

    // Resolve and filter everything before measuring anything, so a mistyped
    // --subject fails now rather than after the benchmarks before it ran.
    let mut plans = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let env = tak_cli::template::env();
    let mut skipped = Vec::new();
    // Subjects a `when` switched off — their own, or their benchmark's — so
    // a --subject naming one is told why it will not run, rather than that
    // it does not exist.
    let mut hidden: std::collections::BTreeMap<String, (String, String)> = Default::default();
    for (name, b) in selected {
        // `when` is decided before anything is rendered, so a skipped
        // benchmark or subject never needs the variables it would have used.
        if let Some(when) = b.when()
            && !tak_cli::condition::eval(when, &env, &name, None)
                .with_context(|| format!("benchmark `{name}`"))?
        {
            for s in cfg.subjects(&name)? {
                hidden
                    .entry(s.name)
                    .or_insert_with(|| (name.clone(), when.to_string()));
            }
            skipped.push((name.clone(), None, when.to_string()));
            continue;
        }
        let subjects = cfg.subjects(&name)?;
        seen.extend(subjects.iter().map(|s| s.name.clone()));
        let mut kept = Vec::with_capacity(subjects.len());
        for s in subjects {
            // Only the subjects being run are decided on: one --subject left
            // out cannot fail the run, whatever its condition evaluates to.
            if !opts.subjects.is_empty() && !opts.subjects.contains(&s.name) {
                continue;
            }
            match &s.when {
                Some(when)
                    if !tak_cli::condition::eval(when, &env, &name, Some(&s.name))
                        .with_context(|| format!("benchmark `{name}`, subject `{}`", s.name))? =>
                {
                    // Remembered rather than fatal: another benchmark may
                    // still measure a subject this one switches off. Only a
                    // requested subject no benchmark runs is an error, below.
                    hidden
                        .entry(s.name.clone())
                        .or_insert_with(|| (name.clone(), when.clone()));
                    skipped.push((name.clone(), Some(s.name.clone()), when.clone()));
                }
                _ => kept.push(s),
            }
        }
        let mut subjects = kept;
        if subjects.is_empty() {
            continue;
        }
        // Filter before rendering: a subject that is not being measured must
        // not fail the run over a variable only it needs.
        if !opts.subjects.is_empty() {
            subjects.retain(|s| opts.subjects.contains(&s.name));
            if subjects.is_empty() {
                continue;
            }
        }
        let mut subjects = subjects
            .into_iter()
            .map(|s| {
                let subject = s.name.clone();
                tak_cli::template::render(s, &name, &env)
                    .with_context(|| format!("benchmark `{name}`, subject `{subject}`"))
            })
            .collect::<Result<Vec<_>>>()?;
        for s in &mut subjects {
            s.anchor(&root);
            // An explicit flag beats the file; the file beats the default.
            s.runs = opts.runs.unwrap_or(s.runs);
            s.warmup = opts.warmup.unwrap_or(s.warmup);
        }
        plans.push((name, b.is_multi(), subjects));
    }
    // A requested subject that no benchmark will measure: say why.
    let planned: std::collections::BTreeSet<&str> = plans
        .iter()
        .flat_map(|(_, _, subjects)| subjects.iter().map(|s| s.name.as_str()))
        .collect();
    if let Some(off) = opts
        .subjects
        .iter()
        .find(|s| !planned.contains(s.as_str()) && hidden.contains_key(*s))
    {
        let (bench, when) = &hidden[off];
        bail!(
            "subject `{off}` was asked for, but `when` is false for it in benchmark `{bench}`: {when}"
        );
    }
    if let Some(missing) = opts.subjects.iter().find(|s| !seen.contains(*s)) {
        bail!(
            "no subject `{missing}` in the selected benchmarks (found: {})",
            seen.into_iter().collect::<Vec<_>>().join(", ")
        );
    }

    for (bench, subject, when) in &skipped {
        let what = subject
            .as_ref()
            .map_or(bench.to_string(), |s| format!("{bench} ({s})"));
        eprintln!("  skipping {what}: `when` is false: {when}");
    }

    if plans.is_empty() {
        if skipped.is_empty() {
            println!("{} declares no benchmarks", path.display());
            return Ok(());
        }
        // Asked to leave a result behind and producing none is a failure: a
        // CI job recording or exporting must not look like it measured
        // something when every benchmark was switched off.
        // A dry run writes neither, so there is nothing for it to fail over.
        if !opts.dry_run && (opts.record || opts.export_json.is_some()) {
            bail!(
                "nothing to {}: every selected benchmark or subject has a false `when`",
                if opts.record { "record" } else { "export" }
            );
        }
        println!("nothing to run: every selected benchmark or subject has a false `when`");
        return Ok(());
    }

    if opts.dry_run {
        println!("{}", path.display());
        for (name, multi, subjects) in &plans {
            print_plan(name, *multi, subjects, opts.no_counters);
        }
        return Ok(());
    }

    let seed = opts.seed.unwrap_or_else(random_seed);
    let mut measured = Vec::new();
    let mut failed = Vec::new();
    for (name, multi, subjects) in &plans {
        let (m, f) = measure_bench(name, subjects, *multi, seed, &opts, settings)?;
        measured.extend(m);
        failed.extend(f);
    }
    finish(measured, failed, &opts, seed, settings)
}

/// A random seed below 2^53, so it survives any JSON reader — JavaScript and
/// jq hold numbers as doubles — and is shorter to copy into `--seed`.
fn random_seed() -> u64 {
    fastrand::u64(..1 << 53)
}

/// Print a benchmark's subjects as they would run, for `tak run --dry-run`:
/// every layer applied, templates rendered, paths anchored and command-line
/// overrides taken into account.
fn print_plan(bench: &str, multi: bool, subjects: &[Subject], no_counters: bool) {
    println!(
        "\n  {bench}{}",
        if multi { "" } else { "  (single command)" }
    );
    for s in subjects {
        if multi {
            println!("    {}", s.name);
        }
        let pad = if multi { "      " } else { "    " };
        println!("{pad}cmd      {}", shell_words(&s.cmd));
        if let Some(p) = &s.prepare {
            println!("{pad}prepare  {}", shell_words(p));
        }
        if let Some(d) = &s.dir {
            println!("{pad}dir      {}", d.display());
        }
        for (k, v) in &s.env {
            println!("{pad}env      {k}={}", shell_words(std::slice::from_ref(v)));
        }
        let runs = match s.runs {
            Runs::Fixed(n) => n.to_string(),
            Runs::Auto => format!(
                "auto ({} budget, {}..={})",
                tak_cli::progress::fmt(s.auto.budget),
                s.auto.min,
                s.auto.max
            ),
        };
        println!("{pad}runs     {runs}, warmup {}", s.warmup);
        // As the run would do it: --no-counters overrides the file.
        if s.counters && !no_counters {
            println!("{pad}counters on");
        }
    }
}

/// Arguments joined for reading, quoting any that a shell would split. For
/// display only: tak itself never passes a command through a shell.
fn shell_words(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Export and record what was measured, then report any failures.
fn finish(
    measured: Vec<Measured>,
    failed: Vec<String>,
    opts: &RunOpts,
    seed: u64,
    settings: &Settings,
) -> Result<()> {
    // The export is written even when a subject failed: it is this run's
    // results, the failed subject is simply absent, and a consumer comparing
    // subjects has to handle a missing one anyway.
    if let Some(path) = &opts.export_json {
        let results = measured
            .iter()
            .map(|m| {
                let command = if m.subject.name == SELF_TOOL {
                    &m.bench
                } else {
                    &m.subject.name
                };
                ExportResult::new(&m.bench, &m.subject.name, command, &m.samples)
            })
            .collect();
        let meta = export::Meta {
            tak_version: env!("CARGO_PKG_VERSION").to_string(),
            seed,
            runner: runner_class(settings),
            time: now_rfc3339(),
        };
        export::write(path, meta, results)?;
        println!(
            "\n  exported {} result(s) to {}",
            measured.len(),
            path.display()
        );
    }
    if !failed.is_empty() {
        // Everything is measured before anything is written, and that holds
        // here too: a run missing a subject is stored whole or not at all,
        // because a partial set left in history looks like a complete one.
        if opts.record {
            eprintln!("\n  not recording: a run with a failed subject would be stored incomplete");
        }
        bail!("{} subject(s) failed: {}", failed.len(), failed.join(", "));
    }
    if opts.record {
        let records: Vec<Record> = measured.into_iter().map(|m| m.record).collect();
        record_all(&records)?;
    }
    Ok(())
}

/// Append every record in one write, so a run is stored whole or not at all.
fn record_all(records: &[Record]) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let sha = notes::rev_parse("HEAD").context("not in a git repository")?;
    notes::append(&sha, records)?;
    println!(
        "\n  recorded {} measurement(s) to {} for {}",
        records.len(),
        notes::NOTES_REF,
        &sha[..12]
    );
    println!("  push with: tak push");
    Ok(())
}

/// Measure one benchmark's subjects, interleaved, and print them.
///
/// A single-command benchmark fails as a whole, as it always has. In a
/// multi-subject one a failing subject is reported and dropped, and its name
/// is returned alongside the subjects that succeeded.
fn measure_bench(
    bench: &str,
    subjects: &[Subject],
    multi: bool,
    seed: u64,
    opts: &RunOpts,
    settings: &Settings,
) -> Result<(Vec<Measured>, Vec<String>)> {
    if multi {
        println!(
            "  {bench}: {} subjects, interleaved (--seed {seed})",
            subjects.len()
        );
    }
    let bench_seed = measure::seed_for(seed, bench);
    let results = if opts.no_progress {
        measure::interleaved(subjects, bench_seed, settings, &mut measure::Quiet)
    } else {
        let names = subjects.iter().map(|s| s.name.clone()).collect();
        let mut bar = tak_cli::progress::Bar::new(bench, names);
        let r = measure::interleaved(subjects, bench_seed, settings, &mut bar);
        bar.finish();
        r
    };
    let mut measured = Vec::new();
    let mut failed = Vec::new();
    for (s, result) in subjects.iter().zip(results) {
        let samples = match result {
            Ok(samples) => samples,
            Err(e) if !multi => return Err(e),
            Err(e) => {
                eprintln!("  error: {bench} ({}) dropped: {e:#}", s.name);
                failed.push(format!("{bench} ({})", s.name));
                continue;
            }
        };
        let mut metrics = measure::stats(&samples);
        let label = if multi {
            format!("{bench} ({})", s.name)
        } else {
            bench.to_string()
        };
        for w in measure::warnings(&samples) {
            eprintln!("  warning: {label}: {w}");
        }
        if s.counters && !opts.no_counters {
            count_into(&mut metrics, s, settings);
        }

        if multi {
            // One line per subject, in the order of a quick read: the floor
            // first, since that is the robust estimator, then the spread.
            // The command is in tak.toml; repeating it here buried the numbers.
            // Instruction counts, when a subject opted in, stay on its line.
            let count = metrics
                .get("instructions")
                .map_or(String::new(), |i| format!("  instructions {i:.0}"));
            println!(
                "    {:<width$}  min {:>9.2}  p50 {:>9.2}  mean {:>9.2} ± {:<8.2} max {:>9.2} ms  n={}{count}",
                s.name,
                metrics["wall_min_ms"],
                metrics["wall_p50_ms"],
                metrics["wall_mean_ms"],
                metrics["wall_stddev_ms"],
                metrics["wall_max_ms"],
                samples.len(),
                width = subjects.iter().map(|s| s.name.len()).max().unwrap_or(0),
            );
        } else {
            println!("  {bench}  {}", s.cmd.join(" "));
        }
        for (k, v) in metrics.iter().filter(|_| !multi) {
            if k == "wall_n" {
                continue;
            }
            if k == "instructions" {
                println!("  {k:<16} {v:>14.0}");
            } else {
                println!("  {k:<16} {v:>14.2}");
            }
        }

        // TAK_TOOL only ever renames the single-command series. Keyed on the
        // benchmark's shape rather than the subject's name, so a declared
        // subject can never be recorded as anything but itself.
        let tool = if multi {
            s.name.clone()
        } else {
            std::env::var("TAK_TOOL").unwrap_or_else(|_| SELF_TOOL.into())
        };
        measured.push(Measured {
            bench: bench.to_string(),
            subject: s.clone(),
            samples,
            record: Record {
                v: SCHEMA_VERSION,
                bench: bench.to_string(),
                tool,
                version: None,
                runner: runner_class(settings),
                ts: now_rfc3339(),
                metrics,
            },
        });
    }
    Ok((measured, failed))
}

/// Add a subject's instruction count to its metrics, warning rather than
/// failing when it cannot be had: the timing already collected is still good.
fn count_into(metrics: &mut BTreeMap<String, f64>, s: &Subject, settings: &Settings) {
    match measure::subject_instructions(s, settings) {
        Ok(Some(c)) => {
            metrics.insert("instructions".into(), c.min as f64);
            if c.is_suspect() {
                eprintln!(
                    "warning: instruction count varied {:.2}% across {} runs. \
                     The metric is deterministic, so this means the command \
                     itself does environment-dependent work (an update check, \
                     a cache it populates on first run, DNS). Its counts are \
                     not a usable gate until that is removed.",
                    c.spread_pct(),
                    c.runs
                );
            }
        }
        Ok(None) => eprintln!(
            "note: valgrind not found — recording timing only. \
             Instruction counts are the only gate-able metric; on macOS/Windows \
             run tak in a Linux container to get them."
        ),
        // Valgrind exists but the measurement failed. Say so rather than
        // blaming a missing install, and keep the timing we did collect.
        Err(e) => eprintln!("warning: instruction counting failed: {e}"),
    }
}

fn cmd_history(rev: String, remote: String) -> Result<()> {
    let sha = notes::rev_parse(&rev).context("not in a git repository")?;
    let recs = notes::read(Some(&remote), &sha)?;
    if recs.is_empty() {
        println!("no measurements recorded for {}", &sha[..12]);
        return Ok(());
    }
    println!("{} measurement(s) for {}\n", recs.len(), &sha[..12]);
    for r in recs {
        let ins = r
            .metrics
            .get("instructions")
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "-".into());
        let wall = r
            .metrics
            .get("wall_min_ms")
            .map(|v| format!("{v:.2}ms"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:<16} {:<10} {:<22} instructions={:<14} wall_min={}",
            r.bench, r.tool, r.runner, ins, wall
        );
    }
    Ok(())
}

/// How many commits of trunk history the sparkline covers.
///
/// A constant rather than a setting: it changes how a picture looks, not what
/// the gate decides, and the registry should hold things worth a project
/// arguing about. Twenty is enough to see a step change and short enough that
/// the column stays narrow in a comment.
const TREND_COMMITS: usize = 20;

/// Recent values for each series along `base`'s first-parent history, oldest
/// first, with `head`'s own value appended.
///
/// Reading the trunk and then the pull request in one line is the point: a
/// number that has drifted for weeks and a number this branch just moved look
/// nothing alike, and the base-versus-head columns alone cannot tell them apart.
fn gather_trend(base: &str, head_sha: &str, head_records: &[Record]) -> Result<compare::Trend> {
    let commits = notes::rev_list(base, TREND_COMMITS)?;
    // rev-list is newest first; a trend reads oldest to newest.
    let mut walked = Vec::with_capacity(commits.len());
    for sha in commits.iter().rev() {
        walked.push((sha.clone(), notes::read(None, sha)?));
    }
    Ok(compare::build_trend(&walked, head_sha, head_records))
}

/// Compare `rev` against `base`, print the report, and gate on it.
fn cmd_compare(
    base: String,
    rev: String,
    remote: String,
    no_gate: bool,
    settings: &Settings,
) -> Result<()> {
    let base_sha = notes::rev_parse(&base).with_context(|| format!("cannot resolve {base}"))?;
    let head_sha = notes::rev_parse(&rev).with_context(|| format!("cannot resolve {rev}"))?;

    // One fetch, not two: `read` refreshes from the remote, and doing it twice
    // doubles the round trip for the same ref.
    let base_records = notes::read(Some(&remote), &base_sha)?;
    let head_records = notes::read(None, &head_sha)?;

    let comparison = compare::compare(&base_records, &head_records);
    // Never fatal: a shallow checkout has no history to walk, and a missing
    // sparkline is a smaller loss than a failed gate.
    let trend = gather_trend(&base_sha, &head_sha, &head_records).unwrap_or_default();
    print!(
        "{}",
        compare::markdown(&comparison, &trend, settings.gate_pct, settings.credit)
    );

    let regressions = comparison.regressions(settings.gate_pct);
    if regressions.is_empty() || no_gate {
        return Ok(());
    }
    // A non-zero exit is the gate. The table above already says which and by
    // how much, so this only has to be unambiguous about why the job failed.
    bail!(
        "{} benchmark(s) regressed by more than {}%",
        regressions.len(),
        settings.gate_pct
    )
}

/// Diagnose the plumbing.
///
/// Takes settings by value so a `tak.toml` that will not parse cannot stop the
/// command whose job is to tell you about it — main resolves them tolerantly
/// and falls back to the defaults.
fn cmd_doctor(settings: &Settings) -> Result<()> {
    println!("tak doctor\n");

    match notes::rev_parse("HEAD") {
        Ok(sha) => println!("  ✓ git repository        HEAD {}", &sha[..12]),
        Err(_) => {
            println!("  ✗ git repository        not in one — nothing can be recorded");
            return Ok(());
        }
    }

    match std::process::Command::new("valgrind")
        .arg("--version")
        .output()
    {
        Ok(o) if o.status.success() => println!(
            "  ✓ valgrind              {}",
            String::from_utf8_lossy(&o.stdout).trim()
        ),
        _ => println!(
            "  ! valgrind              not found — timing only, no gate-able metric\n\
             \x20                         (expected on macOS/Windows; use a Linux container)"
        ),
    }

    match notes::fetch("origin") {
        Ok(true) => println!(
            "  ✓ notes fetch           refreshed {} from origin",
            notes::NOTES_REF
        ),
        Ok(false) => println!(
            "  ! notes fetch           could not fetch {} (no remote, offline, or no data yet)",
            notes::NOTES_REF
        ),
        Err(e) => println!("  ! notes fetch           {e}"),
    }

    println!("  · runner class          {}", runner_class(settings));
    Ok(())
}

/// Infer "owner/name" from the `origin` remote.
fn repo_from_origin() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Match the host exactly. Splitting on the substring "github.com" would
    // accept `git@mygithub.com:o/r` and then query the wrong repository.
    let rest = [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|p| url.strip_prefix(p))?;
    let slug = rest.trim_end_matches('/').trim_end_matches(".git");
    (slug.matches('/').count() == 1 && !slug.is_empty()).then(|| slug.to_string())
}

fn backfill_workdir() -> Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("tak-backfill-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder
        .tempdir()
        .context("could not create a backfill directory")
}

#[allow(clippy::too_many_arguments)]
fn cmd_backfill(
    repo: Option<String>,
    bin: Option<String>,
    args: Vec<String>,
    bench: String,
    limit: usize,
    runs: u32,
    dry_run: bool,
    settings: &Settings,
) -> Result<()> {
    let repo = repo
        .or_else(repo_from_origin)
        .context("could not infer the repository — pass --repo owner/name")?;
    let bin = bin.unwrap_or_else(|| repo.rsplit('/').next().unwrap_or(&repo).to_string());
    let args = if args.is_empty() {
        vec!["--version".to_string()]
    } else {
        args
    };

    // Fail before spending minutes downloading and measuring, and so that a
    // missing tag later means exactly that rather than "not in a repository".
    if !dry_run && !backfill::in_git_repo() {
        bail!(
            "not inside a git repository — measurements are recorded against tagged \
             commits. Run from a clone of {repo}, or pass --dry-run."
        );
    }

    let releases = backfill::list_releases(&repo, limit)?;
    if releases.is_empty() {
        println!("no releases with downloadable assets found for {repo}");
        return Ok(());
    }
    println!("{} release(s) from {repo}\n", releases.len());

    // The parent has to be atomically created and, on Unix, owner-only. A
    // predictable `/tmp/tak-backfill-<pid>` let another local user pre-create
    // that path and replace a downloaded executable before tak spawned it.
    // TempDir also removes downloads on every exit path, including `?`.
    let workdir = backfill_workdir()?;
    let mut recorded = 0usize;
    let mut skipped = 0usize;

    for (i, rel) in releases.iter().enumerate() {
        let Some(asset) = backfill::pick_asset(&rel.assets) else {
            println!("  {:<14} skipped — no asset for this platform", rel.tag);
            skipped += 1;
            continue;
        };

        let dir = workdir.path().join(backfill::release_dir_name(i, &rel.tag));
        let path = match backfill::fetch_binary(asset, &bin, &dir) {
            Ok(p) => p,
            Err(e) => {
                println!("  {:<14} skipped — {e}", rel.tag);
                skipped += 1;
                continue;
            }
        };

        let mut cmd = vec![path.to_string_lossy().to_string()];
        cmd.extend(args.iter().cloned());

        let plan = Plan {
            cmd: cmd.clone(),
            warmup: 2,
            runs,
            // Release binaries are extracted to absolute paths.
            dir: None,
            settings: settings.clone(),
        };
        let mut metrics = match measure::wall(&plan) {
            Ok(m) => m,
            Err(e) => {
                println!("  {:<14} skipped — {e}", rel.tag);
                skipped += 1;
                continue;
            }
        };
        let mut suspect = None;
        if let Ok(Some(c)) = measure::instructions(&cmd, None, settings) {
            metrics.insert("instructions".into(), c.min as f64);
            if c.is_suspect() {
                suspect = Some(c.spread_pct());
            }
        }

        let ins = metrics
            .get("instructions")
            .map(|v| format!("{v:>14.0}"))
            .unwrap_or_else(|| format!("{:>14}", "-"));
        println!(
            "  {:<14} wall_min {:>8.2}ms   instructions {ins}{}",
            rel.tag,
            metrics["wall_min_ms"],
            suspect
                .map(|p| format!("   ⚠ varied {p:.1}%"))
                .unwrap_or_default()
        );

        if dry_run {
            continue;
        }

        // Attach to the tagged commit so the series lands on the real timeline.
        // A shallow clone has no tags, which is a skip rather than an error.
        let Some(sha) = backfill::tag_commit(&rel.tag) else {
            println!(
                "                 not recorded — tag {} not present locally (try `git fetch --tags`)",
                rel.tag
            );
            skipped += 1;
            continue;
        };

        let rec = Record {
            v: SCHEMA_VERSION,
            bench: bench.clone(),
            tool: bin.clone(),
            version: Some(backfill::version_of(&rel.tag).to_string()),
            runner: runner_class(settings),
            // The release's own date, not now: this is when the code existed.
            ts: rel.published_at.clone().unwrap_or_else(now_rfc3339),
            metrics,
        };
        notes::append(&sha, &[rec])?;
        recorded += 1;
    }

    if dry_run {
        println!("\n  dry run — nothing written");
    } else {
        println!(
            "\n  recorded {recorded}, skipped {skipped} → {}",
            notes::NOTES_REF
        );
        println!("  push with: tak push");
    }
    Ok(())
}

/// Resolve settings from the CLI layer, the environment, and `tak.toml`.
///
/// Called only by the commands that measure something or report settings.
/// `push`, `init`, `history` and `doctor` do not consult `tak.toml` at all, so a
/// broken one cannot stop you from pushing measurements you already took.
///
/// Reads only the dotted keys the registry binds to `tak.toml`, so an invalid
/// `[bench.x]` does not abort `tak run -- somecmd` — an explicit command has
/// never depended on the declared benchmarks and still does not.
///
/// A *missing* `tak.toml` is fine. A *syntax-broken* one is an error even here:
/// it may carry `[env]` settings that change what gets scrubbed from a
/// subject's environment, and silently applying a weaker filter than the
/// project asked for is not a good failure.
fn resolve_settings(cli: &CliLayer) -> Result<Settings> {
    Settings::from_process(cli)
}

/// A declared default, written the way `tak.toml` would spell it.
fn render_default(value: &usage_rs::config::Const) -> String {
    use usage_rs::config::{Const, Value};
    match value {
        Const::Str(s) => format!("{s:?}"),
        Const::Bool(b) => b.to_string(),
        Const::Int(i) => i.to_string(),
        Const::Float(f) => Value::Float(*f).display(),
        Const::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(render_default)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_value().display(),
    }
}

/// Print the settings registry with resolved values.
fn cmd_settings(resolved: &Settings, docs: bool) -> Result<()> {
    let scrubbed: Vec<&str> = resolved.scrubbed_env().collect();
    for meta in Settings::SETTINGS_PROPS {
        // `display_value` rather than a match here: a test asserts it answers
        // for every registry entry, so a new setting cannot reach this display
        // without a value behind it.
        let Some(value) = resolved.display_value(meta.key) else {
            println!(
                "{}  (no accessor — wire it into Settings::display_value)",
                meta.key
            );
            continue;
        };
        println!("{}  {}", meta.key, meta.ty.name());
        println!("  value    {value}");
        if let Some(default) = &meta.default {
            println!("  default  {}", render_default(default));
        }
        if !meta.cli.is_empty() {
            println!("  cli      {}", meta.cli.join(", "));
        }
        if !meta.envs.is_empty() {
            println!("  env      {}", meta.envs.join(", "));
        }
        let config_keys: Vec<&str> = meta
            .bindings
            .iter()
            .filter(|(kind, _)| *kind == config_source().name())
            .map(|(_, key)| *key)
            .collect();
        if !config_keys.is_empty() {
            println!("  tak.toml {}", config_keys.join(", "));
        }
        if let Some(since) = meta.since {
            println!("  since    {since}");
        }
        if docs {
            println!();
            if let Some(text) = meta.long_help.or(meta.help) {
                for line in text.trim().lines() {
                    println!("  {line}");
                }
            }
            for example in meta.examples {
                println!("  $ {example}");
            }
        }
        println!();
    }
    println!("removed from measured commands: {scrubbed:?}");
    Ok(())
}

fn main() -> Result<()> {
    // The settings layer beside the parsed struct: what was given on the
    // command line contributes, and what was left off does not.
    let (cli, overrides) = Cli::parse_with_settings();
    match cli.cmd {
        Cmd::Completion { shell } => {
            let shell = usage_rs::complete::Shell::from_name(&shell)
                .ok_or_else(|| anyhow::anyhow!("unsupported shell: {shell}"))?;
            print!("{}", Cli::completion_script(shell));
            Ok(())
        }
        Cmd::Run {
            bench,
            runs,
            warmup,
            no_counters,
            record,
            no_progress,
            subject,
            seed,
            export_json,
            config,
            dry_run,
            cmd,
        } => {
            let settings = Settings::from_process_at(&overrides, config.as_deref())?;
            cmd_run(
                RunOpts {
                    bench,
                    runs: runs.as_deref().map(str::parse).transpose()?,
                    warmup,
                    no_counters,
                    record,
                    no_progress,
                    subjects: subject,
                    seed,
                    export_json,
                    config,
                    dry_run,
                },
                cmd,
                &settings,
            )
        }
        Cmd::History { rev, remote } => cmd_history(rev, remote),
        Cmd::Push(RemoteArgs { remote }) => {
            notes::push(&remote)?;
            println!("pushed {} to {}", notes::NOTES_REF, remote);
            Ok(())
        }
        Cmd::Artifact(args) => match args.cmd {
            ArtifactCmd::Export { output, rev } => {
                let (sha, records) = tak_cli::artifact::export(&output, &rev)?;
                println!(
                    "exported {records} measurement(s) for {} to {}",
                    &sha[..12],
                    output.display()
                );
                Ok(())
            }
            ArtifactCmd::Publish {
                path,
                expect,
                remote,
            } => {
                let (sha, records) = tak_cli::artifact::publish(&path, &expect, &remote)?;
                println!(
                    "published {records} measurement(s) for {} to {remote}",
                    &sha[..12]
                );
                Ok(())
            }
        },
        Cmd::Init(RemoteArgs { remote }) => {
            notes::install_refspec(&remote)?;
            println!(
                "added {} to remote.{}.fetch — plain `git fetch` now picks up measurements",
                notes::NOTES_REF,
                remote
            );
            Ok(())
        }
        Cmd::Backfill {
            repo,
            bin,
            args,
            bench,
            limit,
            runs,
            dry_run,
        } => cmd_backfill(
            repo,
            bin,
            args,
            bench,
            limit,
            runs,
            dry_run,
            &resolve_settings(&overrides)?,
        ),
        Cmd::Compare {
            base,
            rev,
            remote,
            no_gate,
        } => cmd_compare(base, rev, remote, no_gate, &resolve_settings(&overrides)?),
        // Tolerant on purpose: doctor diagnoses a broken setup, so a tak.toml
        // it cannot read must not stop it from running. Falling all the way
        // back to the defaults threw away the flag and the environment too, so
        // doctor reported a derived runner class while a recording would have
        // used the one the user asked for.
        Cmd::Doctor => {
            let resolved = resolve_settings(&overrides).unwrap_or_else(|_| {
                eprintln!("warning: could not read tak.toml; showing settings without it");
                Settings::resolve(
                    &overrides,
                    &EnvLayer::from_process(),
                    &TakConfigLayer::empty(),
                )
                .unwrap_or_default()
            });
            cmd_doctor(&resolved)
        }
        Cmd::Settings { docs } => cmd_settings(&resolve_settings(&overrides)?, docs),
        Cmd::Usage => {
            // The command tree, then the config block: the settings are part
            // of the spec, so docs and completions read the same declaration
            // the resolver does.
            print!("{}", Cli::spec().view().omit_version().to_kdl());
            print!("{}", Settings::spec_kdl());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drift guard. The registry lists the flags each setting declares and
    /// the parser lists the flags it binds to settings; both are generated, and
    /// this holds them against each other — a flag documented and read by
    /// nothing, or read and documented by nothing, fails here.
    ///
    #[test]
    fn the_flags_this_cli_reads_are_the_flags_its_settings_declare() {
        assert_eq!(
            Settings::SETTINGS_REGISTRY.drift(Cli::SETTINGS_BINDINGS),
            Vec::<String>::new()
        );
    }

    #[test]
    fn timestamp_is_rfc3339_shaped() {
        let ts = now_rfc3339();
        assert_eq!(ts.len(), 20, "{ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[cfg(unix)]
    #[test]
    fn the_backfill_workdir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let workdir = backfill_workdir().unwrap();
        let mode = workdir.path().metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "workdir mode was {:o}", mode & 0o777);
    }

    /// An explicit class wins over the derived one. This is how a project
    /// partitions its series across a toolchain bump, which tak cannot detect.
    #[test]
    fn an_explicit_runner_class_wins() {
        let s = Settings {
            runner_class: "gha-linux-x64-rust1.85".into(),
            ..Settings::default()
        };
        assert_eq!(runner_class(&s), "gha-linux-x64-rust1.85");
    }

    /// Empty means derive, and whitespace is empty. Recording under a blank
    /// class would silently merge every machine into one series.
    #[test]
    fn a_blank_class_falls_back_to_the_derived_name() {
        for blank in ["", "   "] {
            let s = Settings {
                runner_class: blank.into(),
                ..Settings::default()
            };
            let got = runner_class(&s);
            assert!(!got.trim().is_empty(), "{blank:?} produced {got:?}");
            assert!(got.contains('-'), "expected a derived name, got {got:?}");
        }
    }
}
