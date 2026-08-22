//! Settings, and where their values come from.
//!
//! The [`Settings`] struct below is the registry: `#[derive(usage_rs::Config)]`
//! generates the metadata slice (`SETTINGS_PROPS`), the resolver registry
//! (`SETTINGS_REGISTRY`), the reader that fills the struct from a resolution,
//! and the spec `config` block that documents it. There is no `settings.toml`
//! and no build-script generator left to keep in step with this file.
//!
//! Precedence, highest first: CLI flag, environment variable, `tak.toml`,
//! declared default. A source that is absent is skipped rather than treated as
//! empty, so setting a value in `tak.toml` is not undone by the flag being
//! unused.

use anyhow::{Context, Result, anyhow};
use std::path::Path;
use usage_rs::config::{
    Layer, LayerCtx, LayerError, LayerOutput, Layers, Origin, SourceKind, Ty, Value,
};

pub use usage_rs::config::{CliLayer, EnvLayer};

/// Every setting tak supports, resolved.
///
/// `PartialEq` but not `Eq`: a float setting has no total equality.
#[derive(usage_rs::Config, Debug, Clone, PartialEq)]
pub struct Settings {
    /// Environment variables removed from every command tak measures.
    ///
    /// Two reasons this defaults to a non-empty list rather than to nothing.
    ///
    /// **Determinism.** A CLI that finds a forge token in its environment often does
    /// more with it than without — authenticating, fetching, checking rate limits. A
    /// measurement that moves depending on whether CI happened to export a token is
    /// not a measurement of the code under test. It lands in the series as an
    /// unexplained step change on the day someone edits an unrelated workflow.
    ///
    /// **Credentials.** `tak backfill` downloads release binaries and executes them,
    /// and any CI run that can push notes has a repository-write token in scope.
    ///
    /// Setting this replaces the default list rather than adding to it. To keep the
    /// defaults and remove more, list them alongside. To keep the defaults and remove
    /// fewer, use `env_allow` — it is subtracted from this list, so the two compose
    /// without either having to restate the other.
    ///
    /// Names are matched exactly. There is no globbing: a benchmark whose behaviour
    /// depends on which variables happen to match a pattern is the problem this
    /// setting exists to avoid.
    ///
    /// tak's own network calls are unaffected. `backfill` authenticates with `curl`
    /// directly rather than through the measurement path.
    #[usage(
        default("GITHUB_TOKEN", "GH_TOKEN"),
        cli("--env-deny"),
        env = "TAK_ENV_DENY",
        parse = "list_by_comma",
        source("config", "env.deny"),
        example("tak run --env-deny AWS_PROFILE --env-deny AWS_REGION"),
        example("TAK_ENV_DENY=GITHUB_TOKEN,GH_TOKEN,NPM_TOKEN tak run"),
        since = "0.0.3"
    )]
    pub env_deny: Vec<String>,

    /// Environment variables kept even though `env_deny` lists them.
    ///
    /// Subtracted from `env_deny`, so a project can opt one variable back in without
    /// restating the whole default list. A CLI whose measured path genuinely requires
    /// a token — a client that cannot start unauthenticated, say — needs this.
    ///
    /// Doing so makes the measurement depend on something outside the repository.
    /// That is a real cost, not a formality: the numbers become conditional on the
    /// environment the run happened to have, and a token expiring will read as a
    /// performance change.
    ///
    /// Listing a variable here that `env_deny` does not mention has no effect. This
    /// setting removes entries from the deny list; it does not add anything to the
    /// environment.
    #[usage(
        default(),
        cli("--env-allow"),
        env = "TAK_ENV_ALLOW",
        parse = "list_by_comma",
        source("config", "env.allow"),
        example("tak run --env-deny GITHUB_TOKEN --env-allow GITHUB_TOKEN"),
        since = "0.0.3"
    )]
    pub env_allow: Vec<String>,

    /// How much an instruction count may rise before `tak compare` fails.
    ///
    /// A percentage of the base measurement. Only instruction counts are gated. Wall
    /// clock is reported and never gated: on the same hardware it moves 4-20% run to
    /// run, so a threshold tight enough to catch a real regression would fire
    /// constantly, and one loose enough to stay quiet would catch nothing.
    ///
    /// The default of 1% is about fifty times the ~0.02% instruction counting
    /// reproduces to, leaving room for the small differences a compiler or dependency
    /// bump can produce without turning the gate into noise.
    ///
    /// Raise it to report without effectively failing. Setting it to zero fails on any
    /// increase at all, which sounds appealing and is not: one extra instruction on a
    /// startup path is not worth blocking a pull request over.
    #[usage(
        default = 1.0,
        cli("--gate-pct"),
        env = "TAK_GATE_PCT",
        source("config", "gate.pct"),
        example("tak compare origin/main --gate-pct 0.5"),
        example("TAK_GATE_PCT=5 tak compare origin/main"),
        since = "0.0.4"
    )]
    pub gate_pct: f64,

    /// Whether generated reports end with a line naming tak.
    ///
    /// On by default. A report that appears in someone's pull request should say what
    /// put it there — a reader who has never heard of tak needs a way to find out, and
    /// a maintainer evaluating the comment needs to know what to turn off.
    ///
    /// Turn it off with `--no-credit`, `TAK_CREDIT=0`, or `credit = false` under
    /// `[report]`. Nothing else about the report changes.
    #[usage(
        default = true,
        cli("--no-credit"),
        env = "TAK_CREDIT",
        source("config", "report.credit"),
        example("tak compare origin/main --no-credit"),
        example("TAK_CREDIT=0 tak compare origin/main"),
        since = "0.0.4"
    )]
    pub credit: bool,

    /// The machine class a measurement is recorded under, and compared within.
    ///
    /// Empty means derive it: `gha-<os>-<arch>` under GitHub Actions, `local-<os>-<arch>`
    /// otherwise. That is right until something about the machine changes without the
    /// name changing.
    ///
    /// Series are partitioned on this, and must be. Absolute instruction counts shift
    /// between machine types by more than a real regression does, so tak will not
    /// compare across classes — it reports the old series as removed and the new one as
    /// added rather than inventing a step change.
    ///
    /// Set it when the *environment* changes in a way the derived name cannot see. The
    /// common case is a toolchain bump: a hosted runner image or a compiler upgrade
    /// between the base measurement and this one is attributed to the code otherwise,
    /// and on a one-percent gate that is a false failure. Encoding the compiler version
    /// into the class starts a fresh series at the bump, which is honest — the numbers
    /// either side genuinely are not comparable.
    ///
    /// tak cannot detect this for you. It measures programs, not build systems, and has
    /// no way to know what produced the binary it is timing.
    #[usage(
        default = "",
        default_note = "derived from the machine",
        cli("--runner"),
        env = "TAK_RUNNER",
        source("config", "runner.class"),
        example("TAK_RUNNER=gha-linux-x64-rust1.85 tak run --record"),
        example("tak run --runner gha-linux-x64-glibc2.39"),
        since = "0.0.6"
    )]
    pub runner_class: String,
}

/// The source kind `tak.toml` contributes under, for `source(...)` bindings
/// and for [`TakConfigLayer`].
pub fn config_source() -> SourceKind {
    SourceKind::new("config")
}

/// `tak.toml` as a settings layer.
///
/// Not usage's `FileLayer`: `tak.toml` is tak's general config file, so most of
/// what it holds — `[bench]` above all — is not a setting, and scanning the file
/// would warn about every one of those keys. This reads the other way around:
/// it iterates the registry's `source("config", ...)` bindings and looks each
/// dotted key up in the parsed TOML, so a key nothing declares is simply not
/// looked at.
///
/// A *missing* `tak.toml` is fine. A *syntax-broken* one — or a declared key
/// holding the wrong type — is an error rather than a warning: the file may
/// carry `[env]` settings that change what gets scrubbed from a subject's
/// environment, and quietly applying a weaker filter than the project asked
/// for is not a good failure.
pub struct TakConfigLayer {
    /// The file that was found, if any, and its parsed contents.
    found: Option<(std::path::PathBuf, toml::Table)>,
}

impl TakConfigLayer {
    /// Find and parse `tak.toml`, searching upward from `start`.
    ///
    /// Walking up means settings resolve the same from a subdirectory as from
    /// the repository root, exactly like `Config::find`.
    pub fn find(start: &Path) -> Result<Self> {
        for dir in start.ancestors() {
            let path = dir.join(crate::config::FILE_NAME);
            if path.is_file() {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("could not read {}", path.display()))?;
                let table: toml::Table = text
                    .parse()
                    .with_context(|| format!("could not parse {}", path.display()))?;
                return Ok(Self {
                    found: Some((path, table)),
                });
            }
        }
        Ok(Self { found: None })
    }

    /// No file at all — what a missing `tak.toml` resolves with, and what
    /// `doctor` falls back to when the file cannot be read.
    pub fn empty() -> Self {
        Self { found: None }
    }

    /// A layer over literal TOML text, for tests.
    #[cfg(test)]
    fn from_text(text: &str) -> Self {
        Self {
            found: Some((
                std::path::PathBuf::from("tak.toml"),
                text.parse().expect("test TOML parses"),
            )),
        }
    }
}

/// A `toml::Value` as the resolver's own value type.
fn value_of(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Int(*i),
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::List(items.iter().map(value_of).collect()),
        toml::Value::Table(entries) => Value::Map(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), value_of(v)))
                .collect(),
        ),
    }
}

/// Whether a TOML value is written as the declared type, before any coercion.
///
/// The resolver's coercion is deliberately forgiving — `deny = "X"` would become
/// a one-item list, `credit = "yes"` would become `true`. tak's config file has
/// always been stricter than that: a value of the wrong TOML type is an error,
/// not a guess, because guessing here changes what gets scrubbed from a
/// subject's environment without saying so.
fn written_as(ty: &Ty, v: &toml::Value) -> bool {
    match ty {
        Ty::Bool => v.is_bool(),
        Ty::Int | Ty::Uint => v.is_integer(),
        Ty::Float => v.is_float() || v.is_integer(),
        Ty::String | Ty::Path | Ty::Url | Ty::Duration => v.is_str(),
        Ty::List(item) | Ty::Set(item) => v
            .as_array()
            .is_some_and(|items| items.iter().all(|item_value| written_as(item, item_value))),
        Ty::Map(value) => v
            .as_table()
            .is_some_and(|entries| entries.values().all(|entry| written_as(value, entry))),
        Ty::Option(inner) => written_as(inner, v),
        _ => true,
    }
}

impl Layer for TakConfigLayer {
    fn source(&self) -> SourceKind {
        config_source()
    }

    fn load(&self, ctx: &LayerCtx) -> Result<LayerOutput, LayerError> {
        let mut out = LayerOutput::new();
        let Some((path, table)) = &self.found else {
            return Ok(out);
        };
        let registry = ctx.registry();
        for (id, config_key) in registry.bindings(self.source()) {
            // Walk the dotted key. An absent table defers like an absent key;
            // a *present* name that is not a table means the file says
            // something this cannot read, which is an error like any other
            // wrong type here.
            let mut parts = config_key.split('.');
            let mut current = table.get(parts.next().unwrap_or_default());
            for part in parts {
                current = match current {
                    None => break,
                    Some(toml::Value::Table(t)) => t.get(part),
                    Some(_) => {
                        return Err(LayerError::Unreadable {
                            source: path.display().to_string(),
                            why: format!("`{config_key}` is not a table of settings"),
                        });
                    }
                };
            }
            let Some(raw) = current else {
                continue;
            };
            let meta = registry.get(id);
            if !written_as(&meta.ty, raw) {
                return Err(LayerError::Unreadable {
                    source: path.display().to_string(),
                    why: format!("`{config_key}` expected {}", meta.ty.describe()),
                });
            }
            let origin = Origin::new(self.source(), path.display().to_string());
            match ctx.entry_from_value(meta.key, value_of(raw), origin) {
                Ok(entry) => out.push(entry),
                // The shape check above should have refused everything the
                // coercion would; anything left is still the file being wrong.
                Err(warning) => {
                    return Err(LayerError::Unreadable {
                        source: path.display().to_string(),
                        why: warning.message,
                    });
                }
            }
        }
        Ok(out)
    }
}

/// A layer with a blank `runner_class` treated as not given at all.
///
/// Blank means the same thing everywhere: derive the class. Treating a blank
/// `--runner ""` or an exported-but-empty `TAK_RUNNER=` as a set value would
/// block every lower-precedence source and record every machine under one
/// empty class — so a blank entry falls through to the next layer, exactly as
/// the hand-written resolver always had it.
struct SkipBlankRunner<'a>(&'a dyn Layer);

impl Layer for SkipBlankRunner<'_> {
    fn source(&self) -> SourceKind {
        self.0.source()
    }

    fn load(&self, ctx: &LayerCtx) -> Result<LayerOutput, LayerError> {
        let mut out = self.0.load(ctx)?;
        let runner = ctx.prop("runner_class").map(|found| found.id);
        out.entries.retain(|entry| {
            Some(entry.prop) != runner
                || !matches!(&entry.value, Value::String(s) if s.trim().is_empty())
        });
        Ok(out)
    }
}

impl Default for Settings {
    /// The declared defaults, read the same way any other resolution is.
    fn default() -> Self {
        let resolved = usage_rs::config::resolve(Self::SETTINGS_REGISTRY, Layers::new())
            .expect("no layers were given, so there is nothing to fail");
        Self::read(&resolved).expect("every setting declares a default")
    }
}

impl Settings {
    /// Resolve every setting from the given layers, highest precedence first.
    pub fn resolve(cli: &CliLayer, env: &EnvLayer, config: &TakConfigLayer) -> Result<Self> {
        let cli = SkipBlankRunner(cli);
        let env = SkipBlankRunner(env);
        let config = SkipBlankRunner(config);
        let resolved = usage_rs::config::resolve(
            Self::SETTINGS_REGISTRY,
            Layers::new().then(&cli).then(&env).then(&config),
        )
        .map_err(|e| anyhow!("{e}"))?;
        // A typo must not silently become the default and let a regression
        // through a gate the user thought they set — say so, then proceed with
        // the value the remaining sources produce.
        for warning in usage_rs::config::explain::warnings(&resolved) {
            eprintln!("warning: {warning}");
        }
        let mut settings = Self::read(&resolved).map_err(|e| anyhow!("{e}"))?;
        // `TAK_ENV_DENY=A,,B` never named an empty variable; the hand-written
        // reader dropped blanks and call sites still rely on that.
        settings.env_deny.retain(|name| !name.is_empty());
        settings.env_allow.retain(|name| !name.is_empty());
        Ok(settings)
    }

    /// Resolve against the real process environment and the `tak.toml` found
    /// upward from the current directory.
    pub fn from_process(cli: &CliLayer) -> Result<Self> {
        let config =
            TakConfigLayer::find(&std::env::current_dir()?).context("could not read settings")?;
        Self::resolve(cli, &EnvLayer::from_process(), &config)
    }

    /// The value of a setting, by its registry key.
    ///
    /// Exists so display code cannot silently omit a setting: `SETTINGS_PROPS`
    /// is generated, so a new entry appears in `tak settings` whether or not
    /// anything can produce its value. A test asserts this returns `Some` for
    /// every registry entry, which turns "added a setting, forgot the
    /// accessor" into a build failure instead of a blank row.
    pub fn display_value(&self, name: &str) -> Option<String> {
        match name {
            "env_allow" => Some(format!("{:?}", self.env_allow)),
            "env_deny" => Some(format!("{:?}", self.env_deny)),
            "credit" => Some(format!("{}", self.credit)),
            "runner_class" => Some(if self.runner_class.is_empty() {
                "(derived)".to_string()
            } else {
                self.runner_class.clone()
            }),
            "gate_pct" => Some(format!("{}", self.gate_pct)),
            _ => None,
        }
    }

    /// Variables to remove from a benchmark subject: denied, less allowed.
    ///
    /// Allow subtracts from deny rather than sitting beside it, so opting one
    /// variable back in does not mean restating the whole default list.
    pub fn scrubbed_env(&self) -> impl Iterator<Item = &str> {
        self.env_deny
            .iter()
            .filter(|name| !self.env_allow.contains(name))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_cli() -> CliLayer {
        CliLayer::new(std::iter::empty::<(String, String)>())
    }

    fn no_env() -> EnvLayer {
        EnvLayer::new(std::iter::empty::<(String, String)>())
    }

    fn env(vars: &[(&str, &str)]) -> EnvLayer {
        EnvLayer::new(
            vars.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn the_default_protects_forge_tokens() {
        let s = Settings::default();
        let scrubbed: Vec<_> = s.scrubbed_env().collect();
        assert!(scrubbed.contains(&"GITHUB_TOKEN"));
        assert!(scrubbed.contains(&"GH_TOKEN"));
    }

    #[test]
    fn allow_subtracts_from_deny() {
        let s = Settings {
            env_deny: vec!["A".into(), "B".into()],
            env_allow: vec!["B".into()],
            ..Settings::default()
        };
        assert_eq!(s.scrubbed_env().collect::<Vec<_>>(), ["A"]);
    }

    /// Allowing something that is not denied is a no-op, not an error and not
    /// an addition — this setting only ever removes entries from the deny list.
    #[test]
    fn allowing_an_undenied_variable_does_nothing() {
        let s = Settings {
            env_deny: vec!["A".into()],
            env_allow: vec!["ZZZ".into()],
            ..Settings::default()
        };
        assert_eq!(s.scrubbed_env().collect::<Vec<_>>(), ["A"]);
    }

    #[test]
    fn cli_beats_env_beats_config() {
        let cfg = TakConfigLayer::from_text("[env]\ndeny = [\"FROM_CONFIG\"]\n");
        let with_env = env(&[("TAK_ENV_DENY", "FROM_ENV")]);

        let from_config = Settings::resolve(&no_cli(), &no_env(), &cfg).unwrap();
        assert_eq!(from_config.env_deny, ["FROM_CONFIG"]);

        let from_env = Settings::resolve(&no_cli(), &with_env, &cfg).unwrap();
        assert_eq!(from_env.env_deny, ["FROM_ENV"]);

        let cli = no_cli().with_value("env_deny", Value::List(vec![Value::from("FROM_CLI")]));
        let from_cli = Settings::resolve(&cli, &with_env, &cfg).unwrap();
        assert_eq!(from_cli.env_deny, ["FROM_CLI"]);
    }

    #[test]
    fn an_absent_source_defers_rather_than_clearing() {
        let cfg = TakConfigLayer::from_text("[env]\ndeny = [\"FROM_CONFIG\"]\n");
        // No CLI flag and no variable: the config value survives.
        let s = Settings::resolve(&no_cli(), &no_env(), &cfg).unwrap();
        assert_eq!(s.env_deny, ["FROM_CONFIG"]);
    }

    /// An exported-but-empty variable is a deliberate empty list. Falling
    /// through to `tak.toml` here would make `TAK_ENV_DENY=` silently do the
    /// opposite of what it looks like.
    #[test]
    fn an_empty_variable_means_an_empty_list() {
        let cfg = TakConfigLayer::from_text("[env]\ndeny = [\"FROM_CONFIG\"]\n");
        let s = Settings::resolve(&no_cli(), &env(&[("TAK_ENV_DENY", "")]), &cfg).unwrap();
        assert!(s.env_deny.is_empty());
    }

    #[test]
    fn a_variable_is_split_on_commas_and_trimmed() {
        let with_env = env(&[("TAK_ENV_DENY", " A , B ,, C ")]);
        let s = Settings::resolve(&no_cli(), &with_env, &TakConfigLayer::empty()).unwrap();
        assert_eq!(s.env_deny, ["A", "B", "C"]);
    }

    /// A blank flag must defer, like a blank variable and a blank config key.
    /// Otherwise `--runner ""` blocks every lower-precedence source and records
    /// under an empty class, merging every machine into one series.
    #[test]
    fn a_blank_cli_runner_falls_through() {
        let cfg = TakConfigLayer::from_text("[runner]\nclass = \"from-config\"\n");
        let cli = no_cli().with("runner_class", "   ");
        let s = Settings::resolve(&cli, &no_env(), &cfg).unwrap();
        assert_eq!(s.runner_class, "from-config");
    }

    /// The same for the environment: exported-but-empty means "derive it".
    #[test]
    fn a_blank_runner_variable_falls_through() {
        let cfg = TakConfigLayer::from_text("[runner]\nclass = \"from-config\"\n");
        let s = Settings::resolve(&no_cli(), &env(&[("TAK_RUNNER", "")]), &cfg).unwrap();
        assert_eq!(s.runner_class, "from-config");
    }

    /// Keys in `tak.toml` that are not settings — `[bench]` above all — are
    /// none of the resolver's business and must not produce warnings or
    /// errors. The layer reads the registry's bindings, not the file's keys.
    #[test]
    fn non_setting_keys_are_not_looked_at() {
        let cfg = TakConfigLayer::from_text(
            "[bench.startup]\ncmd = \"./x --version\"\n[gate]\npct = 0.5\n",
        );
        let s = Settings::resolve(&no_cli(), &no_env(), &cfg).unwrap();
        assert_eq!(s.gate_pct, 0.5);
    }

    /// A declared key holding the wrong TOML type is an error, not a guess.
    /// The resolver's coercion would read `deny = "X"` as a one-item list;
    /// tak's config file has always been stricter, because guessing here
    /// changes what gets scrubbed from a subject's environment.
    #[test]
    fn a_wrongly_typed_config_key_is_an_error() {
        let cfg = TakConfigLayer::from_text("[env]\ndeny = \"not a list\"\n");
        let err = Settings::resolve(&no_cli(), &no_env(), &cfg).unwrap_err();
        assert!(format!("{err:#}").contains("env.deny"), "{err:#}");
    }

    /// The drift guard, half one: every declared environment variable actually
    /// changes the resolved settings. A sentinel that differs from every
    /// default and parses as every declared type — a list sees `["12345"]`, a
    /// float sees `12345`; booleans get the opposite of their default.
    #[test]
    fn every_declared_env_var_is_honoured() {
        for meta in Settings::SETTINGS_PROPS {
            for var in meta.envs {
                let sentinel = if meta.ty == Ty::Bool {
                    "false"
                } else {
                    "12345"
                };
                let with_env = env(&[(var, sentinel)]);
                let got =
                    Settings::resolve(&no_cli(), &with_env, &TakConfigLayer::empty()).unwrap();
                assert_ne!(
                    got,
                    Settings::default(),
                    "`{}` declares {var} but setting it changes nothing",
                    meta.key
                );
            }
        }
    }

    /// The TOML literal for a sentinel of this registry type.
    fn config_sentinel(ty: &Ty) -> String {
        match ty {
            Ty::List(_) => "[\"SENTINEL\"]".to_string(),
            Ty::Float => "12345.0".to_string(),
            // The opposite of every bool default, so flipping it always shows.
            Ty::Bool => "false".to_string(),
            Ty::String => "\"SENTINEL\"".to_string(),
            other => panic!(
                "the drift check has no sentinel for type `{}`",
                other.name()
            ),
        }
    }

    /// The drift guard, half two: every declared `tak.toml` key reaches its
    /// field. A dotted registry key is valid TOML on its own, so this builds
    /// the smallest config that sets exactly that key and checks it lands.
    #[test]
    fn every_declared_config_key_is_honoured() {
        let kind = config_source();
        for meta in Settings::SETTINGS_PROPS {
            for (source, key) in meta.bindings {
                if *source != kind.name() {
                    continue;
                }
                let text = format!("{key} = {}\n", config_sentinel(&meta.ty));
                let cfg = TakConfigLayer::from_text(&text);
                let got = Settings::resolve(&no_cli(), &no_env(), &cfg).unwrap();
                assert_ne!(
                    got,
                    Settings::default(),
                    "`{}` declares config key `{key}` but setting it changes nothing",
                    meta.key
                );
            }
        }
    }

    /// Display code reads values by registry key, and `SETTINGS_PROPS` is
    /// generated, so a new setting shows up in `tak settings` whether or not
    /// its value can be produced. This is what stops that being a blank row.
    #[test]
    fn every_setting_has_an_accessor() {
        let s = Settings::default();
        for meta in Settings::SETTINGS_PROPS {
            assert!(
                s.display_value(meta.key).is_some(),
                "`{}` has no accessor in Settings::display_value",
                meta.key
            );
        }
    }

    /// Every setting must be reachable somehow, or it is documentation for a
    /// feature that does not exist.
    #[test]
    fn every_setting_declares_a_source() {
        for meta in Settings::SETTINGS_PROPS {
            assert!(
                !meta.cli.is_empty() || !meta.envs.is_empty() || !meta.bindings.is_empty(),
                "`{}` has no sources",
                meta.key
            );
        }
    }
}
