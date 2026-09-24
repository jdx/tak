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

/// Exit codes a sample may end with and still count, unless `ok_exit_codes`
/// says otherwise: success, and nothing else, as a shell would judge it.
pub const DEFAULT_OK_EXIT_CODES: [i32; 1] = [0];

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Benchmarks by name. A BTreeMap so runs are ordered and reproducible
    /// rather than following the file's incidental key order.
    ///
    /// Settings tables — `[env]`, `[gate]`, `[report]`, `[runner]` — live in
    /// the same file but are not deserialized here: the settings registry
    /// declares their dotted keys, and `settings::TakConfigLayer` reads exactly
    /// those, so this type never has to be kept in step with it. That is also
    /// why defaults for every benchmark live under `[defaults]`, not `[env]`.
    #[serde(default)]
    pub bench: BTreeMap<String, Bench>,
    /// Settings every benchmark starts from.
    #[serde(default)]
    defaults: Layer,
    /// Subjects declared once, for benchmarks to name in `subjects = [...]`.
    #[serde(default)]
    subject: BTreeMap<String, SubjectDecl>,
}

/// The settings that stack: `[defaults]`, a benchmark, a shared subject and a
/// benchmark's own subject table all carry them, and each overrides the one
/// before. `env` and `vars` merge key by key rather than replacing.
#[derive(Debug, Default, Deserialize)]
struct Layer {
    /// A count, or `"auto"` to size each subject from its own speed.
    runs: Option<RunsDecl>,
    warmup: Option<u32>,
    /// `runs = "auto"`: wall time to spend per subject, as `30s`, `2m`, `500ms`.
    budget: Option<String>,
    /// `runs = "auto"`: fewest runs any subject gets.
    min_runs: Option<u32>,
    /// `runs = "auto"`: most runs any subject gets.
    max_runs: Option<u32>,
    /// Exit codes that count as a successful sample. Replaces, not merges:
    /// a subject listing `[0, 1]` means exactly those.
    ok_exit_codes: Option<Vec<i64>>,
    /// Untimed command run before every sample.
    prepare: Option<Cmd>,
    /// Directory to run in, relative to `tak.toml`.
    dir: Option<PathBuf>,
    /// Variables set for the command.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Values for templates, as `{{ vars.name }}`. Not passed to the command.
    #[serde(default)]
    vars: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct Bench {
    /// The command, for a benchmark of one program. Mutually exclusive with
    /// `subjects` and `subject`, which declare several programs measured
    /// against each other.
    #[serde(default)]
    cmd: Option<Cmd>,
    #[serde(flatten)]
    layer: Layer,
    /// Run this benchmark only when this expr condition holds.
    when: Option<String>,
    /// Shared `[subject.NAME]` tables this benchmark measures.
    #[serde(default)]
    subjects: Vec<String>,
    /// Subjects declared here, or overrides of shared ones for this benchmark
    /// only. The name is what their measurements are recorded under
    /// (`Record::tool`). A BTreeMap for the same reason `Config::bench` is one.
    #[serde(default)]
    subject: BTreeMap<String, SubjectDecl>,
}

/// One program in a multi-subject benchmark, as written in `tak.toml`: either
/// a shared `[subject.NAME]` or a benchmark's `[bench.B.subject.NAME]`.
#[derive(Debug, Deserialize)]
pub struct SubjectDecl {
    /// Optional here because a benchmark's table may only override settings of
    /// a shared subject that already has one.
    #[serde(default)]
    cmd: Option<Cmd>,
    #[serde(flatten)]
    layer: Layer,
    /// Measure this subject only when this expr condition holds. A
    /// benchmark's own table replaces a shared subject's condition.
    when: Option<String>,
    /// Opt in to instruction counting. Off by default because a
    /// multi-subject benchmark usually compares against other people's
    /// programs, whose instruction counts are not this project's to gate on:
    /// a competitor's upgrade would fail the gate.
    counters: Option<bool>,
}

/// A command, written either as a list or as a plain string.
#[derive(Debug, Clone, Deserialize)]
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
            Cmd::Line(s) => split_line(s),
        };
        if v.is_empty() {
            bail!("empty command");
        }
        Ok(v)
    }
}

/// Split a string command on whitespace, keeping each template tag whole:
/// `mycli {{ vars.lockfile }}` is two arguments, the second rendered later.
/// Nothing else is special — no quotes, no escapes — as for any string
/// command.
fn split_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut close: Option<&str> = None;
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if let Some(end) = close {
            if rest.starts_with(end) {
                cur.push_str(end);
                rest = &rest[end.len()..];
                close = None;
                continue;
            }
        } else if let Some(end) = [("{{", "}}"), ("{%", "%}"), ("{#", "#}")]
            .iter()
            .find_map(|(open, end)| rest.starts_with(open).then_some(*end))
        {
            cur.push_str(&rest[..2]);
            rest = &rest[2..];
            close = Some(end);
            continue;
        } else if c.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            rest = &rest[c.len_utf8()..];
            continue;
        }
        cur.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
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
    /// Template values from `vars` tables, used by [`crate::template`].
    pub vars: BTreeMap<String, String>,
    /// Measured only when this holds; see [`crate::condition`].
    pub when: Option<String>,
    pub runs: Runs,
    /// The limits `Runs::Auto` works within.
    pub auto: AutoRuns,
    pub warmup: u32,
    pub counters: bool,
    /// Exit codes a timed or warmup sample may end with and still count.
    /// Never empty. A death by signal fails whatever this holds, and
    /// `prepare` is always held to exit 0.
    pub ok_exit_codes: Vec<i32>,
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
    /// This benchmark's own `when` condition.
    pub fn when(&self) -> Option<&str> {
        self.when.as_deref()
    }

    /// Whether this benchmark compares several programs rather than measuring
    /// one.
    pub fn is_multi(&self) -> bool {
        !self.subject.is_empty() || !self.subjects.is_empty()
    }
}

impl Config {
    /// Every subject of benchmark `name`, with each layer applied: defaults,
    /// the benchmark, the shared subject, then the benchmark's own subject
    /// table. Templates are not rendered yet; see [`crate::template::render`].
    ///
    /// A single-command benchmark is one subject named [`SELF_TOOL`] with
    /// counters on, which is exactly what it measured before subjects existed.
    pub fn subjects(&self, name: &str) -> Result<Vec<Subject>> {
        let b = self
            .bench
            .get(name)
            .with_context(|| format!("no benchmark `{name}`"))?;
        if let Some(cmd) = &b.cmd {
            if b.is_multi() {
                bail!("declares both `cmd` and subjects; use one or the other");
            }
            return Ok(vec![resolve(
                SELF_TOOL,
                cmd,
                true,
                None,
                &[&self.defaults, &b.layer],
            )?]);
        }
        // A benchmark's own table can add a subject or override a shared one;
        // either way it is measured, listed or not.
        let names: std::collections::BTreeSet<&String> =
            b.subjects.iter().chain(b.subject.keys()).collect();
        if names.is_empty() {
            bail!("has no command");
        }
        names
            .into_iter()
            .map(|n| {
                let shared = self.subject.get(n);
                let local = b.subject.get(n);
                if b.subjects.contains(n) && shared.is_none() && local.is_none() {
                    bail!("lists subject `{n}`, but there is no [subject.{n}]");
                }
                let cmd = local
                    .and_then(|d| d.cmd.as_ref())
                    .or_else(|| shared.and_then(|d| d.cmd.as_ref()))
                    .with_context(|| format!("subject `{n}` has no command"))?;
                let when = local
                    .and_then(|d| d.when.clone())
                    .or_else(|| shared.and_then(|d| d.when.clone()));
                let counters = local
                    .and_then(|d| d.counters)
                    .or_else(|| shared.and_then(|d| d.counters))
                    .unwrap_or(false);
                let mut layers = vec![&self.defaults, &b.layer];
                layers.extend(shared.map(|d| &d.layer));
                layers.extend(local.map(|d| &d.layer));
                resolve(n, cmd, counters, when, &layers).with_context(|| format!("subject `{n}`"))
            })
            .collect()
    }
}

impl Config {
    /// Every settings layer in the file, with where it is.
    fn layers(&self) -> Vec<(String, &Layer)> {
        let mut out = vec![("defaults".to_string(), &self.defaults)];
        for (n, d) in &self.subject {
            out.push((format!("subject.{n}"), &d.layer));
        }
        for (b, bench) in &self.bench {
            out.push((format!("bench.{b}"), &bench.layer));
            for (n, d) in &bench.subject {
                out.push((format!("bench.{b}.subject.{n}"), &d.layer));
            }
        }
        out
    }

    /// Every `when` in the file, with where it is.
    fn conditions(&self) -> Vec<(String, &str)> {
        let mut out = Vec::new();
        for (n, d) in &self.subject {
            out.extend(d.when.as_deref().map(|w| (format!("subject.{n}.when"), w)));
        }
        for (b, bench) in &self.bench {
            out.extend(
                bench
                    .when
                    .as_deref()
                    .map(|w| (format!("bench.{b}.when"), w)),
            );
            for (n, d) in &bench.subject {
                out.extend(
                    d.when
                        .as_deref()
                        .map(|w| (format!("bench.{b}.subject.{n}.when"), w)),
                );
            }
        }
        out
    }

    /// Every string that may hold a template, as written, with where it is.
    fn template_strings(&self) -> Vec<(String, &str)> {
        fn layer<'a>(out: &mut Vec<(String, &'a str)>, at: &str, l: &'a Layer) {
            if let Some(p) = &l.prepare {
                cmd(out, &format!("{at}.prepare"), p);
            }
            if let Some(d) = l.dir.as_ref().and_then(|d| d.to_str()) {
                out.push((format!("{at}.dir"), d));
            }
            for (k, v) in &l.env {
                out.push((format!("{at}.env.{k}"), v));
            }
            for (k, v) in &l.vars {
                out.push((format!("{at}.vars.{k}"), v));
            }
        }
        fn cmd<'a>(out: &mut Vec<(String, &'a str)>, at: &str, c: &'a Cmd) {
            match c {
                Cmd::Argv(v) => out.extend(v.iter().map(|a| (at.to_string(), a.as_str()))),
                Cmd::Line(l) => out.push((at.to_string(), l)),
            }
        }
        fn subject<'a>(out: &mut Vec<(String, &'a str)>, at: &str, d: &'a SubjectDecl) {
            if let Some(c) = &d.cmd {
                cmd(out, &format!("{at}.cmd"), c);
            }
            layer(out, at, &d.layer);
        }
        let mut out = Vec::new();
        layer(&mut out, "defaults", &self.defaults);
        for (n, d) in &self.subject {
            subject(&mut out, &format!("subject.{n}"), d);
        }
        for (b, bench) in &self.bench {
            if let Some(c) = &bench.cmd {
                cmd(&mut out, &format!("bench.{b}.cmd"), c);
            }
            layer(&mut out, &format!("bench.{b}"), &bench.layer);
            for (n, d) in &bench.subject {
                subject(&mut out, &format!("bench.{b}.subject.{n}"), d);
            }
        }
        out
    }
}

/// Stack `layers`, least specific first, into one subject. A later layer's
/// setting replaces an earlier one's — a subject's own prepare replaces the
/// benchmark's rather than running after it, since the two usually reset the
/// same state — except `env` and `vars`, which merge key by key.
fn resolve(
    name: &str,
    cmd: &Cmd,
    counters: bool,
    when: Option<String>,
    layers: &[&Layer],
) -> Result<Subject> {
    fn last<'a, T>(layers: &[&'a Layer], f: impl Fn(&'a Layer) -> Option<&'a T>) -> Option<&'a T> {
        layers.iter().rev().find_map(|l| f(l))
    }
    let merged = |f: fn(&Layer) -> &BTreeMap<String, String>| {
        layers.iter().fold(BTreeMap::new(), |mut m, l| {
            m.extend(f(l).clone());
            m
        })
    };
    let budget = match last(layers, |l| l.budget.as_ref()) {
        Some(b) => parse_duration(b).context("budget")?,
        None => DEFAULT_BUDGET,
    };
    let auto = AutoRuns {
        budget,
        min: last(layers, |l| l.min_runs.as_ref())
            .copied()
            .unwrap_or(DEFAULT_MIN_RUNS),
        max: last(layers, |l| l.max_runs.as_ref())
            .copied()
            .unwrap_or(DEFAULT_MAX_RUNS),
    };
    if auto.min == 0 {
        bail!("min_runs must be at least 1");
    }
    if auto.min > auto.max {
        bail!(
            "min_runs ({}) is more than max_runs ({})",
            auto.min,
            auto.max
        );
    }
    Ok(Subject {
        name: name.to_string(),
        cmd: cmd.argv()?,
        prepare: last(layers, |l| l.prepare.as_ref())
            .map(Cmd::argv)
            .transpose()?,
        dir: last(layers, |l| l.dir.as_ref()).cloned(),
        env: merged(|l| &l.env),
        vars: merged(|l| &l.vars),
        when,
        runs: last(layers, |l| l.runs.as_ref())
            .map_or(Ok(Runs::Fixed(DEFAULT_RUNS)), RunsDecl::resolve)?,
        auto,
        warmup: last(layers, |l| l.warmup.as_ref())
            .copied()
            .unwrap_or(DEFAULT_WARMUP),
        counters,
        ok_exit_codes: match last(layers, |l| l.ok_exit_codes.as_ref()) {
            Some(codes) => ok_exit_codes(codes)?,
            None => DEFAULT_OK_EXIT_CODES.to_vec(),
        },
    })
}

/// Check `ok_exit_codes` as written, returning it sorted and deduplicated.
///
/// The range is whatever `ExitStatus::code()` can report, which is an `i32`.
/// Unix only ever reports 0–255, but Windows passes a program's 32-bit exit
/// code through, and an NTSTATUS such as 0xC0000005 arrives negative
/// (-1073741819); limiting the list to 0–255 would make those unlistable.
/// An empty list would fail every sample, so it is an error here rather than
/// a confusing run.
fn ok_exit_codes(codes: &[i64]) -> Result<Vec<i32>> {
    if codes.is_empty() {
        bail!("ok_exit_codes must list at least one exit code");
    }
    let mut out = codes
        .iter()
        .map(|&c| match i32::try_from(c) {
            Ok(c) => Ok(c),
            Err(_) => bail!(
                "ok_exit_codes: {c} is not an exit code ({} to {})",
                i32::MIN,
                i32::MAX
            ),
        })
        .collect::<Result<Vec<_>>>()?;
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).context("could not parse tak.toml")?;
        // Every template in the file, used or not: a typo in a shared subject
        // no benchmark lists yet, or in a value an override replaces, is
        // still a typo, and should not wait to be found until it is used.
        for (place, value) in cfg.template_strings() {
            crate::template::check_str(value).with_context(|| format!("in {place}"))?;
        }
        for (place, when) in cfg.conditions() {
            crate::condition::check(when).with_context(|| format!("in {place}"))?;
        }
        // Likewise every `ok_exit_codes`, including one in a layer that
        // nothing resolves through yet.
        for (place, l) in cfg.layers() {
            if let Some(codes) = &l.ok_exit_codes {
                ok_exit_codes(codes).with_context(|| format!("in {place}"))?;
            }
        }
        // Every declared benchmark is validated up front rather than failing
        // partway through a run that has already spent minutes measuring.
        for (name, b) in &cfg.bench {
            let subjects = cfg
                .subjects(name)
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

    /// Load a named config file, for `tak run --config`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
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
        let mut s = c.subjects(bench).unwrap();
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
        assert_eq!(only(&c, "a").runs, Runs::Fixed(DEFAULT_RUNS));
        assert_eq!(only(&c, "a").warmup, DEFAULT_WARMUP);
    }

    #[test]
    fn per_benchmark_overrides_win() {
        let c = Config::parse("[bench.a]\ncmd = \"x\"\nruns = 5\nwarmup = 1").unwrap();
        assert_eq!(only(&c, "a").runs, Runs::Fixed(5));
        assert_eq!(only(&c, "a").warmup, 1);
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
        let s = c.subjects("install").unwrap();
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
        .subjects("a")
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
            .subjects("a")
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
        let s = c.subjects("b").unwrap();
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

    /// A subject declared once can be measured by several benchmarks, which
    /// can still override it for themselves; `[defaults]` is under all of it.
    #[test]
    fn shared_subjects_and_defaults_stack_by_specificity() {
        let c = Config::parse(
            r#"
            [defaults]
            runs = "auto"
            min_runs = 4
            prepare = "reset"
            env = { HOME = "/h", CI = "1" }
            vars = { kind = "default" }

            [subject.aube]
            cmd = ["aube", "install"]
            env = { HOME = "/h-aube" }
            vars = { lockfile = "aube-lock.yaml" }

            [subject.pnpm]
            cmd = ["pnpm", "install"]
            counters = true

            [bench.warm]
            subjects = ["aube", "pnpm"]

            [bench.cold]
            subjects = ["aube", "pnpm"]
            prepare = "reset --cold"
            warmup = 0

            [bench.test]
            subjects = ["pnpm"]
            [bench.test.subject.aube]
            cmd = ["aube", "test"]
            [bench.test.subject.pnpm]
            cmd = ["pnpm", "install-test"]
            vars = { kind = "local" }
            "#,
        )
        .unwrap();

        let warm = c.subjects("warm").unwrap();
        assert_eq!(
            warm.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["aube", "pnpm"]
        );
        assert_eq!(warm[0].cmd, ["aube", "install"]);
        assert_eq!(warm[0].runs, Runs::Auto);
        assert_eq!(warm[0].auto.min, 4);
        assert_eq!(warm[0].prepare.as_deref().unwrap(), ["reset"]);
        assert_eq!(warm[0].env["HOME"], "/h-aube", "subject over defaults");
        assert_eq!(warm[0].env["CI"], "1", "merged, not replaced");
        assert_eq!(warm[0].vars["lockfile"], "aube-lock.yaml");
        assert_eq!(warm[0].vars["kind"], "default");
        assert!(!warm[0].counters && warm[1].counters);

        let cold = c.subjects("cold").unwrap();
        assert_eq!(
            cold[0].prepare.as_deref().unwrap(),
            ["reset", "--cold"],
            "bench over defaults"
        );
        assert_eq!(cold[0].warmup, 0);

        // A benchmark's own table adds a subject, or overrides a shared one.
        let test = c.subjects("test").unwrap();
        assert_eq!(test[0].cmd, ["aube", "test"]);
        assert_eq!(
            test[0].env["HOME"], "/h-aube",
            "inherits the shared subject"
        );
        assert_eq!(test[1].cmd, ["pnpm", "install-test"]);
        assert_eq!(test[1].vars["kind"], "local");
        assert!(test[1].counters, "an override keeps the shared setting");
    }

    #[test]
    fn a_listed_subject_must_exist() {
        let err = Config::parse("[bench.a]\nsubjects = [\"ghost\"]").unwrap_err();
        assert!(format!("{err:#}").contains("[subject.ghost]"), "{err:#}");
    }

    #[test]
    fn a_subject_needs_a_command_from_somewhere() {
        let err = Config::parse("[bench.a.subject.x]\nruns = 3").unwrap_err();
        assert!(format!("{err:#}").contains("no command"), "{err:#}");
    }

    #[test]
    fn a_broken_template_is_rejected_at_parse_time() {
        let err = Config::parse("[bench.a]\ncmd = [\"x\", \"{{ env.X \"]").unwrap_err();
        assert!(format!("{err:#}").contains("template"), "{err:#}");
    }

    /// A subject a benchmark both lists and declares itself is fine without a
    /// shared table: the local one provides everything.
    #[test]
    fn a_listed_subject_may_be_declared_locally() {
        let c = Config::parse("[bench.b]\nsubjects = [\"x\"]\n[bench.b.subject.x]\ncmd = [\"x\"]")
            .unwrap();
        assert_eq!(c.subjects("b").unwrap()[0].cmd, ["x"]);
    }

    /// Only exit 0 counts unless a layer says otherwise, and the most specific
    /// layer's list replaces the others' rather than adding to them.
    #[test]
    fn ok_exit_codes_default_to_zero_and_stack_by_replacing() {
        let c = Config::parse(
            r#"
            [defaults]
            ok_exit_codes = [0, 1]

            [subject.grep]
            cmd = ["grep", "x", "file"]
            ok_exit_codes = [1, 0, 1]

            [subject.lint]
            cmd = ["lint"]

            [bench.plain]
            cmd = "x"

            [bench.cmp]
            subjects = ["grep", "lint"]
            ok_exit_codes = [0, 2]

            [bench.strict]
            subjects = ["grep"]
            [bench.strict.subject.grep]
            ok_exit_codes = [0]
            "#,
        )
        .unwrap();
        assert_eq!(only(&c, "plain").ok_exit_codes, [0, 1], "from defaults");
        let cmp = c.subjects("cmp").unwrap();
        assert_eq!(
            cmp[0].ok_exit_codes,
            [0, 1],
            "shared subject, sorted and deduplicated"
        );
        assert_eq!(cmp[1].ok_exit_codes, [0, 2], "bench over defaults");
        assert_eq!(
            only(&c, "strict").ok_exit_codes,
            [0],
            "bench's own subject table"
        );

        let bare = Config::parse("[bench.a]\ncmd = \"x\"").unwrap();
        assert_eq!(only(&bare, "a").ok_exit_codes, DEFAULT_OK_EXIT_CODES);
    }

    /// Windows reports exit codes beyond 0–255, and an NTSTATUS such as
    /// 0xC0000005 as a negative `i32`, so the whole `i32` range is accepted.
    #[test]
    fn ok_exit_codes_take_any_i32() {
        let c = Config::parse(
            "[bench.a]\ncmd = \"x\"\nok_exit_codes = [0, 256, -1073741819, 2147483647]",
        )
        .unwrap();
        assert_eq!(
            only(&c, "a").ok_exit_codes,
            [-1073741819, 0, 256, 2147483647]
        );
    }

    /// A list that could never match a real exit status would fail every
    /// sample, so it is rejected before anything runs — even in a layer no
    /// benchmark uses yet.
    #[test]
    fn bad_ok_exit_codes_are_rejected_at_parse_time() {
        for bad in [
            "ok_exit_codes = []",
            "ok_exit_codes = [2147483648]",
            "ok_exit_codes = [-2147483649]",
            "ok_exit_codes = [0xC0000005]",
            "ok_exit_codes = [\"1\"]",
            "ok_exit_codes = 1",
        ] {
            let toml = format!("[bench.a]\ncmd = \"x\"\n{bad}");
            assert!(Config::parse(&toml).is_err(), "accepted: {bad}");
        }
        let unused =
            "[subject.spare]\ncmd = \"y\"\nok_exit_codes = [4294967296]\n[bench.a]\ncmd = \"x\"";
        let err = Config::parse(unused).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("subject.spare") && msg.contains("4294967296"),
            "{msg}"
        );
    }

    /// The syntax check covers the whole file, not only what some benchmark
    /// ends up using.
    #[test]
    fn an_unused_broken_template_is_still_rejected() {
        let unused = "[subject.spare]\ncmd = [\"{{ env.X \"]\n[bench.b]\ncmd = [\"x\"]";
        assert!(Config::parse(unused).is_err(), "unlisted shared subject");
        let overridden = "[subject.a]\ncmd = [\"{% if %}\"]\n[bench.b]\nsubjects = [\"a\"]\n[bench.b.subject.a]\ncmd = [\"x\"]";
        assert!(
            Config::parse(overridden).is_err(),
            "shared value an override replaces"
        );
    }
}
