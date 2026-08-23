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
    cmd: Cmd,
    pub runs: Option<u32>,
    pub warmup: Option<u32>,
}

/// A command, written either as a list or as a plain string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Cmd {
    Argv(Vec<String>),
    Line(String),
}

impl Bench {
    /// The command as argv.
    ///
    /// A string is split on whitespace and nothing else. There is deliberately
    /// no shell: spawning one adds its own startup cost and variance to every
    /// sample, which for commands in the 10ms range is a large fraction of the
    /// measurement. Anything needing a pipe or a glob should be a list whose
    /// first element is the interpreter.
    pub fn argv(&self) -> Result<Vec<String>> {
        let v = match &self.cmd {
            Cmd::Argv(v) => v.clone(),
            Cmd::Line(s) => s.split_whitespace().map(str::to_string).collect(),
        };
        if v.is_empty() {
            bail!("empty command");
        }
        Ok(v)
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
            b.argv()
                .with_context(|| format!("benchmark `{name}` has no command"))?;
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
        assert_eq!(c.bench["a"].argv().unwrap(), ["mycli", "--version"]);
        assert_eq!(c.bench["b"].argv().unwrap(), ["mycli", "--help"]);
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
        assert_eq!(c.bench["a"].argv().unwrap(), ["mycli", "'two", "words'"]);
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
}
