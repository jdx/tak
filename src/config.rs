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

pub const FILE_NAME: &str = "tak.toml";

/// Defaults chosen to match `tak run`'s, so moving a command into `tak.toml`
/// does not silently change what it measures.
pub const DEFAULT_RUNS: u32 = 20;
pub const DEFAULT_WARMUP: u32 = 3;

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
    pub runs: Option<u32>,
    pub warmup: Option<u32>,
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
    runs: Option<u32>,
    warmup: Option<u32>,
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
    pub runs: u32,
    pub warmup: u32,
    pub counters: bool,
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
                runs: self.runs(),
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
                            runs: s.runs.unwrap_or_else(|| self.runs()),
                            warmup: s.warmup.unwrap_or_else(|| self.warmup()),
                            counters: s.counters,
                        })
                    };
                    resolve().with_context(|| format!("subject `{name}`"))
                })
                .collect(),
        }
    }

    pub fn runs(&self) -> u32 {
        self.runs.unwrap_or(DEFAULT_RUNS)
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
                if s.runs == 0 {
                    bail!("benchmark `{name}`: runs must be at least 1");
                }
                if s.name.trim().is_empty() {
                    bail!("benchmark `{name}`: a subject needs a name");
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
        assert_eq!(c.bench["a"].runs(), DEFAULT_RUNS);
        assert_eq!(c.bench["a"].warmup(), DEFAULT_WARMUP);
    }

    #[test]
    fn per_benchmark_overrides_win() {
        let c = Config::parse("[bench.a]\ncmd = \"x\"\nruns = 5\nwarmup = 1").unwrap();
        assert_eq!(c.bench["a"].runs(), 5);
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
        assert_eq!((s[0].runs, s[0].warmup), (6, 2));
        assert!(!s[0].counters, "named subjects count only when asked to");

        assert_eq!(
            s[1].prepare.as_deref().unwrap(),
            ["rm", "-rf", "node_modules", "pnpm-lock.yaml"]
        );
        assert_eq!(s[1].dir.as_deref(), Some(Path::new("fixture-pnpm")));
        assert_eq!(s[1].env["HOME"], "/tmp/shared");
        assert_eq!((s[1].runs, s[1].warmup), (3, 0));
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
}
