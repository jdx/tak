//! `tak.toml` — declared benchmarks.
//!
//! Named after the tool rather than after its current contents. It already
//! holds more than a command list in spirit, and gates, runner classes and
//! competitor definitions all belong here too; `bench.toml` would be misnamed
//! the moment the first of those lands.
//!
//! The point of declaring benchmarks is that CI and a laptop run the same
//! thing. A command line in a workflow file drifts from the one people use
//! locally, and the numbers stop being comparable without anyone noticing.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const FILE_NAME: &str = "tak.toml";

/// Defaults chosen to match `tak run`'s, so moving a command into `tak.toml`
/// does not silently change what it measures.
pub const DEFAULT_RUNS: u32 = 20;
pub const DEFAULT_WARMUP: u32 = 3;

/// `runs = "auto"` defaults: about this much wall time per subject, prepare
/// included, and never fewer or more runs than these. The floor keeps a slow
/// competitor's result from resting on one or two samples; the ceiling stops
/// a millisecond-scale subject from spending the whole budget on runs that
/// stopped adding information long before.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(30);
pub const DEFAULT_MIN_RUNS: u32 = 5;
pub const DEFAULT_MAX_RUNS: u32 = 50;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Benchmarks by name. A BTreeMap so runs are ordered and reproducible
    /// rather than following the file's incidental key order.
    ///
    /// Settings tables — `[env]`, `[gate]`, `[report]`, `[runner]` — live in
    /// the same file but are not deserialized here: the settings registry
    /// declares their dotted keys, and `settings::TakConfigLayer` reads exactly
    /// those, so this type never has to be kept in step with it.
    #[serde(default)]
    pub bench: BTreeMap<String, Bench>,
}

#[derive(Debug, Deserialize)]
pub struct Bench {
    /// The command, for a benchmark of one program. Mutually exclusive with
    /// `subject`, which declares several programs measured against each other.
    #[serde(default)]
    cmd: Option<Cmd>,
    /// A count, or `"auto"` to size each subject from its own speed.
    pub runs: Option<RunsDecl>,
    pub warmup: Option<u32>,
    /// `runs = "auto"`: wall time to spend per subject, as `30s`, `2m`, `500ms`.
    pub budget: Option<String>,
    /// `runs = "auto"`: fewest runs any subject gets.
    pub min_runs: Option<u32>,
    /// `runs = "auto"`: most runs any subject gets.
    pub max_runs: Option<u32>,
    /// Untimed command run before every sample. On a multi-subject benchmark
    /// this is the default for subjects that do not declare their own.
    #[serde(default)]
    prepare: Option<Cmd>,
    /// Directory to run in, relative to `tak.toml`.
    #[serde(default)]
    dir: Option<PathBuf>,
    /// Variables set for the command. Subjects inherit these and may override
    /// individual keys.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Programs measured against each other, by name. The name is what their
    /// measurements are recorded under (`Record::tool`). A BTreeMap for the
    /// same reason `Config::bench` is one.
    #[serde(default)]
    subject: BTreeMap<String, SubjectDecl>,
}

/// One program in a multi-subject benchmark, as written in `tak.toml`.
#[derive(Debug, Deserialize)]
pub struct SubjectDecl {
    cmd: Cmd,
    #[serde(default)]
    prepare: Option<Cmd>,
    #[serde(default)]
    dir: Option<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    runs: Option<RunsDecl>,
    warmup: Option<u32>,
    budget: Option<String>,
    min_runs: Option<u32>,
    max_runs: Option<u32>,
    /// Opt in to instruction counting. Off by default because a
    /// multi-subject benchmark usually compares against other people's
    /// programs, whose instruction counts are not this project's to gate on:
    /// a competitor's upgrade would fail the gate.
    #[serde(default)]
    counters: bool,
}

/// A command, written either as a list or as a plain string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Cmd {
    Argv(Vec<String>),
    Line(String),
}

impl Cmd {
    /// The command as argv.
    ///
    /// A string is split on whitespace and nothing else. There is deliberately
    /// no shell: spawning one adds its own startup cost and variance to every
    /// sample, which for commands in the 10ms range is a large fraction of the
    /// measurement. Anything needing a pipe or a glob should be a list whose
    /// first element is the interpreter.
    fn argv(&self) -> Result<Vec<String>> {
        let v = match self {
            Cmd::Argv(v) => v.clone(),
            Cmd::Line(s) => s.split_whitespace().map(str::to_string).collect(),
        };
        if v.is_empty() {
            bail!("empty command");
        }
        Ok(v)
    }
}

/// `runs` as written: a count, or a word. Only `"auto"` is a valid word,
/// checked when the subject is resolved so the error names it.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RunsDecl {
    Count(u32),
    Word(String),
}

impl RunsDecl {
    fn resolve(&self) -> Result<Runs> {
        match self {
            RunsDecl::Count(n) => Ok(Runs::Fixed(*n)),
            RunsDecl::Word(w) if w == "auto" => Ok(Runs::Auto),
            RunsDecl::Word(w) => bail!("runs must be a number or \"auto\", not {w:?}"),
        }
    }
}

/// How many timed runs a subject gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runs {
    Fixed(u32),
    /// Decided after the warmups, from how long one sample takes: see
    /// [`AutoRuns`].
    Auto,
}

impl std::str::FromStr for Runs {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        if s == "auto" {
            return Ok(Runs::Auto);
        }
        s.parse()
            .map(Runs::Fixed)
            .map_err(|_| anyhow::anyhow!("runs must be a number or \"auto\", not {s:?}"))
    }
}

/// The limits `runs = "auto"` works within. Resolved for every subject, so
/// `tak run --runs auto` can switch a fixed benchmark over without the file
/// having to declare them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoRuns {
    pub budget: Duration,
    pub min: u32,
    pub max: u32,
}

impl AutoRuns {
    /// Runs for a subject whose samples take `slot` each, prepare included:
    /// as many as fit in the budget, within `min..=max`.
    pub fn runs_for(&self, slot: Duration) -> u32 {
        let fit = if slot.is_zero() {
            u64::from(self.max)
        } else {
            (self.budget.as_secs_f64() / slot.as_secs_f64()) as u64
        };
        fit.clamp(u64::from(self.min), u64::from(self.max)) as u32
    }
}

/// Parse `500ms`, `30s`, `2m` or `1h`; a bare number is seconds.
pub fn parse_duration(text: &str) -> Result<Duration> {
    let t = text.trim();
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: f64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("not a duration: {text:?} (try `30s` or `2m`)"))?;
    let secs = match unit.trim() {
        "ms" => n / 1000.0,
        "" | "s" => n,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        _ => bail!("not a duration: {text:?} (units are ms, s, m, h)"),
    };
    if !secs.is_finite() || secs <= 0.0 {
        bail!("a duration must be positive: {text:?}");
    }
    // Finite and positive can still be too large for a Duration; that is a
    // configuration error, not a panic.
    Duration::try_from_secs_f64(secs).map_err(|_| anyhow::anyhow!("duration too large: {text:?}"))
}

/// The name a single-command benchmark records under. `compare` renders this
/// tool as the bare benchmark name.
pub const SELF_TOOL: &str = "self";

/// A subject with every default applied, ready to measure.
#[derive(Debug, Clone, PartialEq)]
pub struct Subject {
    /// Recorded as `Record::tool`.
    pub name: String,
    pub cmd: Vec<String>,
    pub prepare: Option<Vec<String>>,
    /// Relative to `tak.toml`; the caller resolves it.
    pub dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub runs: Runs,
    /// The limits `Runs::Auto` works within.
    pub auto: AutoRuns,
    pub warmup: u32,
    pub counters: bool,
}

impl Subject {
    /// Resolve paths against `root`, the directory holding `tak.toml`.
    ///
    /// `dir` becomes the working directory, but a relative program path — one
    /// containing a `/`, like `./target/release/mycli` — is resolved against
    /// `root`, not `dir`. Otherwise giving a subject a fixture directory would
    /// make the project's own binary unfindable. A bare name is left for PATH
    /// lookup, and arguments are never touched.
    pub fn anchor(&mut self, root: &Path) {
        let program = |argv: &mut Vec<String>| {
            if let Some(p) = argv.first_mut()
                && p.contains('/')
                && Path::new(p.as_str()).is_relative()
            {
                *p = root.join(p.as_str()).to_string_lossy().into_owned();
            }
        };
        program(&mut self.cmd);
        if let Some(prepare) = &mut self.prepare {
            program(prepare);
        }
        self.dir = Some(match &self.dir {
            Some(d) => root.join(d),
            None => root.to_path_buf(),
        });
    }
}

impl Bench {
    /// Whether this benchmark compares several programs rather than measuring
    /// one.
    pub fn is_multi(&self) -> bool {
        !self.subject.is_empty()
    }

    /// Every subject with the benchmark's defaults applied.
    ///
    /// A single-command benchmark is one subject named [`SELF_TOOL`] with
    /// counters on, which is exactly what it measured before subjects existed.
    pub fn subjects(&self) -> Result<Vec<Subject>> {
        match (&self.cmd, self.subject.is_empty()) {
            (Some(_), false) => bail!("declares both `cmd` and `subject`; use one or the other"),
            (None, true) => bail!("has no command"),
            (Some(cmd), true) => Ok(vec![Subject {
                name: SELF_TOOL.to_string(),
                cmd: cmd.argv()?,
                prepare: self.prepare.as_ref().map(Cmd::argv).transpose()?,
                dir: self.dir.clone(),
                env: self.env.clone(),
                runs: self.runs()?,
                auto: self.auto(None, None, None)?,
                warmup: self.warmup(),
                counters: true,
            }]),
            (None, false) => self
                .subject
                .iter()
                .map(|(name, s)| {
                    let resolve = || -> Result<Subject> {
                        // A subject's own prepare replaces the benchmark's rather
                        // than running after it: the two usually reset the same
                        // state, and running both would double the untimed cost.
                        let prepare = s.prepare.as_ref().or(self.prepare.as_ref());
                        let mut env = self.env.clone();
                        env.extend(s.env.clone());
                        Ok(Subject {
                            name: name.clone(),
                            cmd: s.cmd.argv()?,
                            prepare: prepare.map(Cmd::argv).transpose()?,
                            dir: s.dir.clone().or_else(|| self.dir.clone()),
                            env,
                            runs: match &s.runs {
                                Some(r) => r.resolve()?,
                                None => self.runs()?,
                            },
                            auto: self.auto(s.budget.as_deref(), s.min_runs, s.max_runs)?,
                            warmup: s.warmup.unwrap_or_else(|| self.warmup()),
                            counters: s.counters,
                        })
                    };
                    resolve().with_context(|| format!("subject `{name}`"))
                })
                .collect(),
        }
    }

    pub fn runs(&self) -> Result<Runs> {
        self.runs
            .as_ref()
            .map_or(Ok(Runs::Fixed(DEFAULT_RUNS)), RunsDecl::resolve)
    }

    /// The auto-run limits, a subject's own values over the benchmark's over
    /// the defaults.
    fn auto(&self, budget: Option<&str>, min: Option<u32>, max: Option<u32>) -> Result<AutoRuns> {
        let budget = match budget.or(self.budget.as_deref()) {
            Some(b) => parse_duration(b).context("budget")?,
            None => DEFAULT_BUDGET,
        };
        let a = AutoRuns {
            budget,
            min: min.or(self.min_runs).unwrap_or(DEFAULT_MIN_RUNS),
            max: max.or(self.max_runs).unwrap_or(DEFAULT_MAX_RUNS),
        };
        if a.min == 0 {
            bail!("min_runs must be at least 1");
        }
        if a.min > a.max {
            bail!("min_runs ({}) is more than max_runs ({})", a.min, a.max);
        }
        Ok(a)
    }

    pub fn warmup(&self) -> u32 {
        self.warmup.unwrap_or(DEFAULT_WARMUP)
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).context("could not parse tak.toml")?;
        // Every declared benchmark is validated up front rather than failing
        // partway through a run that has already spent minutes measuring.
        for (name, b) in &cfg.bench {
            let subjects = b
                .subjects()
                .with_context(|| format!("benchmark `{name}`"))?;
            for s in &subjects {
                // Zero runs leaves nothing to report; catching it here names
                // the benchmark instead of failing after the others ran.
                if s.runs == Runs::Fixed(0) {
                    bail!("benchmark `{name}`: runs must be at least 1");
                }
                if s.name.trim().is_empty() {
                    bail!("benchmark `{name}`: a subject needs a name");
                }
                // `self` is the series a single-command benchmark records
                // under. A subject taking it would be recorded, printed and
                // exported as that series instead of as itself.
                if b.is_multi() && s.name == SELF_TOOL {
                    bail!(
                        "benchmark `{name}`: `{SELF_TOOL}` is reserved and cannot name a subject"
                    );
                }
            }
        }
        Ok(cfg)
    }

    /// Find and load `tak.toml`, searching upward from `start`.
    ///
    /// Walking up means `tak run` behaves the same from a subdirectory as from
    /// the repository root, which is where people actually are.
    pub fn find(start: &Path) -> Result<Option<(PathBuf, Self)>> {
        for dir in start.ancestors() {
            let path = dir.join(FILE_NAME);
            if path.is_file() {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("could not read {}", path.display()))?;
                let cfg = Self::parse(&text).with_context(|| format!("in {}", path.display()))?;
                return Ok(Some((path, cfg)));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single subject of a one-command benchmark.
    fn only(c: &Config, bench: &str) -> Subject {
        let mut s = c.bench[bench].subjects().unwrap();
        assert_eq!(s.len(), 1);
        s.remove(0)
    }

    #[test]
    fn a_command_may_be_a_list_or_a_string() {
        let c = Config::parse(
            r#"
            [bench.a]
            cmd = ["mycli", "--version"]
            [bench.b]
            cmd = "mycli --help"
            "#,
        )
        .unwrap();
        assert_eq!(only(&c, "a").cmd, ["mycli", "--version"]);
        assert_eq!(only(&c, "b").cmd, ["mycli", "--help"]);
    }

    /// A string is split on whitespace and nothing else — no shell means no
    /// quoting, and pretending otherwise would measure the wrong thing.
    #[test]
    fn a_string_command_gets_no_shell_semantics() {
        let c = Config::parse(
            r#"[bench.a]
cmd = "mycli 'two words'""#,
        )
        .unwrap();
        assert_eq!(only(&c, "a").cmd, ["mycli", "'two", "words'"]);
    }

    #[test]
    fn defaults_match_the_cli() {
        let c = Config::parse("[bench.a]\ncmd = \"x\"").unwrap();
        assert_eq!(c.bench["a"].runs().unwrap(), Runs::Fixed(DEFAULT_RUNS));
        assert_eq!(c.bench["a"].warmup(), DEFAULT_WARMUP);
    }

    #[test]
    fn per_benchmark_overrides_win() {
        let c = Config::parse("[bench.a]\ncmd = \"x\"\nruns = 5\nwarmup = 1").unwrap();
        assert_eq!(c.bench["a"].runs().unwrap(), Runs::Fixed(5));
        assert_eq!(c.bench["a"].warmup(), 1);
    }

    /// Validation happens at load, not partway through a run that has already
    /// spent minutes measuring.
    #[test]
    fn an_empty_command_is_rejected_at_parse_time() {
        let err = Config::parse("[bench.a]\ncmd = []").unwrap_err();
        assert!(format!("{err:#}").contains('a'), "{err:#}");
    }

    #[test]
    fn benchmarks_run_in_a_stable_order() {
        let c = Config::parse("[bench.zebra]\ncmd = \"z\"\n[bench.alpha]\ncmd = \"a\"").unwrap();
        assert_eq!(c.bench.keys().collect::<Vec<_>>(), ["alpha", "zebra"]);
    }

    #[test]
    fn an_empty_file_declares_nothing() {
        assert!(Config::parse("").unwrap().bench.is_empty());
    }

    #[test]
    fn find_walks_up_from_a_subdirectory() {
        let root = std::env::temp_dir().join(format!("tak-cfg-{}", std::process::id()));
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(FILE_NAME), "[bench.x]\ncmd = \"true\"").unwrap();

        let (path, cfg) = Config::find(&nested).unwrap().expect("should find it");
        assert_eq!(path, root.join(FILE_NAME));
        assert!(cfg.bench.contains_key("x"));

        std::fs::remove_dir_all(&root).ok();
    }

    /// A single-command benchmark keeps measuring what it did before subjects
    /// existed: one series under `self`, with instruction counts.
    #[test]
    fn a_single_command_is_one_self_subject_with_counters() {
        let c = Config::parse("[bench.a]\ncmd = \"x\"").unwrap();
        let s = only(&c, "a");
        assert_eq!(s.name, SELF_TOOL);
        assert!(s.counters);
        assert!(!c.bench["a"].is_multi());
    }

    #[test]
    fn subjects_inherit_the_benchmark_defaults_and_may_override_them() {
        let c = Config::parse(
            r#"
            [bench.install]
            runs = 6
            warmup = 2
            prepare = ["rm", "-rf", "node_modules"]
            dir = "fixture"
            env = { CI = "1", HOME = "/tmp/shared" }

            [bench.install.subject.aube]
            cmd = "aube install"
            env = { HOME = "/tmp/aube" }

            [bench.install.subject.pnpm]
            cmd = ["pnpm", "install"]
            prepare = "rm -rf node_modules pnpm-lock.yaml"
            dir = "fixture-pnpm"
            runs = 3
            warmup = 0
            counters = true
            "#,
        )
        .unwrap();
        let b = &c.bench["install"];
        assert!(b.is_multi());
        let s = b.subjects().unwrap();
        // BTreeMap order, not file order.
        assert_eq!(s[0].name, "aube");
        assert_eq!(s[1].name, "pnpm");

        assert_eq!(s[0].cmd, ["aube", "install"]);
        assert_eq!(
            s[0].prepare.as_deref().unwrap(),
            ["rm", "-rf", "node_modules"]
        );
        assert_eq!(s[0].dir.as_deref(), Some(Path::new("fixture")));
        assert_eq!(s[0].env["CI"], "1");
        assert_eq!(s[0].env["HOME"], "/tmp/aube");
        assert_eq!((s[0].runs, s[0].warmup), (Runs::Fixed(6), 2));
        assert!(!s[0].counters, "named subjects count only when asked to");

        assert_eq!(
            s[1].prepare.as_deref().unwrap(),
            ["rm", "-rf", "node_modules", "pnpm-lock.yaml"]
        );
        assert_eq!(s[1].dir.as_deref(), Some(Path::new("fixture-pnpm")));
        assert_eq!(s[1].env["HOME"], "/tmp/shared");
        assert_eq!((s[1].runs, s[1].warmup), (Runs::Fixed(3), 0));
        assert!(s[1].counters);
    }

    /// Declaring both would leave it ambiguous which is measured.
    #[test]
    fn cmd_and_subjects_together_are_rejected() {
        let err =
            Config::parse("[bench.a]\ncmd = \"x\"\n[bench.a.subject.b]\ncmd = \"y\"").unwrap_err();
        assert!(format!("{err:#}").contains("both"), "{err:#}");
    }

    #[test]
    fn a_benchmark_with_neither_is_rejected() {
        let err = Config::parse("[bench.a]\nruns = 3").unwrap_err();
        assert!(format!("{err:#}").contains("no command"), "{err:#}");
    }

    /// The error names the subject, not just the benchmark, so a large
    /// comparison does not have to be bisected by hand.
    #[test]
    fn an_empty_subject_command_is_rejected_by_name() {
        let err =
            Config::parse("[bench.a.subject.good]\ncmd = \"x\"\n[bench.a.subject.bad]\ncmd = []")
                .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("`a`") && msg.contains("`bad`"), "{msg}");
    }

    #[test]
    fn an_empty_prepare_is_rejected() {
        assert!(Config::parse("[bench.a]\ncmd = \"x\"\nprepare = []").is_err());
    }

    #[test]
    fn zero_runs_is_rejected_at_parse_time() {
        assert!(Config::parse("[bench.a.subject.b]\ncmd = \"x\"\nruns = 0").is_err());
    }

    #[test]
    fn self_is_reserved_for_single_command_benchmarks() {
        let err = Config::parse("[bench.a.subject.self]\ncmd = \"x\"").unwrap_err();
        assert!(format!("{err:#}").contains("reserved"), "{err:#}");
    }

    /// `dir` sets where a command runs, not where a relative program path is
    /// found: `./target/release/x` still means the one next to `tak.toml`.
    #[test]
    fn a_relative_program_path_is_anchored_at_the_config() {
        let root = Path::new("/repo");
        let mut s = Config::parse(
            "[bench.a]\ncmd = [\"./target/x\", \"./arg\"]\nprepare = \"bin/reset\"\ndir = \"fix\"",
        )
        .unwrap()
        .bench["a"]
            .subjects()
            .unwrap()
            .remove(0);
        s.anchor(root);
        assert_eq!(
            s.cmd,
            ["/repo/./target/x", "./arg"],
            "arguments are left alone"
        );
        assert_eq!(s.prepare.unwrap(), ["/repo/bin/reset"]);
        assert_eq!(s.dir.unwrap(), Path::new("/repo/fix"));

        let mut bare = Config::parse("[bench.a]\ncmd = \"mycli --version\"")
            .unwrap()
            .bench["a"]
            .subjects()
            .unwrap()
            .remove(0);
        bare.anchor(root);
        assert_eq!(
            bare.cmd[0], "mycli",
            "a bare name is still looked up on PATH"
        );
        assert_eq!(bare.dir.unwrap(), Path::new("/repo"));
    }

    #[test]
    fn runs_may_be_auto_with_limits_inherited_and_overridden() {
        let c = Config::parse(
            r#"
            [bench.b]
            runs = "auto"
            budget = "2m"
            min_runs = 3

            [bench.b.subject.fast]
            cmd = "x"

            [bench.b.subject.slow]
            cmd = "y"
            max_runs = 10
            budget = "500ms"

            [bench.b.subject.pinned]
            cmd = "z"
            runs = 7
            "#,
        )
        .unwrap();
        let s = c.bench["b"].subjects().unwrap();
        let by = |n: &str| s.iter().find(|x| x.name == n).unwrap();
        assert_eq!(by("fast").runs, Runs::Auto);
        assert_eq!(
            by("fast").auto,
            AutoRuns {
                budget: Duration::from_secs(120),
                min: 3,
                max: DEFAULT_MAX_RUNS
            }
        );
        assert_eq!(by("slow").auto.budget, Duration::from_millis(500));
        assert_eq!((by("slow").auto.min, by("slow").auto.max), (3, 10));
        assert_eq!(by("pinned").runs, Runs::Fixed(7));
    }

    #[test]
    fn bad_auto_settings_are_rejected_at_parse_time() {
        for bad in [
            "runs = \"lots\"",
            "runs = \"auto\"\nbudget = \"soon\"",
            "runs = \"auto\"\nbudget = \"0s\"",
            "runs = \"auto\"\nmin_runs = 0",
            "runs = \"auto\"\nmin_runs = 9\nmax_runs = 3",
        ] {
            let toml = format!("[bench.a]\ncmd = \"x\"\n{bad}");
            assert!(Config::parse(&toml).is_err(), "accepted: {bad}");
        }
    }

    #[test]
    fn durations_take_the_common_units() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1.5h").unwrap(), Duration::from_secs(5400));
        assert!(
            parse_duration("1e300h").is_err(),
            "too large is an error, not a panic"
        );
    }

    /// A slow subject is held at the floor, a fast one at the ceiling, and
    /// anything between gets what fits in the budget.
    #[test]
    fn auto_runs_fit_the_budget_within_the_limits() {
        let a = AutoRuns {
            budget: Duration::from_secs(30),
            min: 5,
            max: 50,
        };
        assert_eq!(a.runs_for(Duration::from_millis(1700)), 17);
        assert_eq!(a.runs_for(Duration::from_secs(21)), 5);
        assert_eq!(a.runs_for(Duration::from_millis(300)), 50);
        assert_eq!(a.runs_for(Duration::ZERO), 50);
    }
}
