//! Settings, and where their values come from.
//!
//! `settings.toml` in the repository root is the source of truth. `build.rs`
//! turns it into the [`Settings`] struct, its [`Default`], and the [`SETTINGS`]
//! metadata slice included below. Nothing here restates what a setting *is* —
//! only how a value is chosen for it.
//!
//! Precedence, highest first: CLI flag, environment variable, `tak.toml`,
//! declared default. A source that is absent is skipped rather than treated as
//! empty, so setting a value in `tak.toml` is not undone by the flag being
//! unused.

use crate::config::SettingsSections;

include!(concat!(env!("OUT_DIR"), "/settings_generated.rs"));

/// Values supplied on the command line.
///
/// `None` means the flag was not given, which is not the same as being given an
/// empty list — the first defers to lower-precedence sources, the second would
/// override them. Repeated flags accumulate, and clap yields an empty vector
/// when a flag is absent, so [`from_cli`] does that conversion in one place.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overrides {
    pub env_deny: Option<Vec<String>>,
    pub env_allow: Option<Vec<String>>,
    pub gate_pct: Option<f64>,
    pub credit: Option<bool>,
}

/// Treat an empty vector from clap as "flag not given".
///
/// A consequence worth knowing: there is no way to clear a list from the
/// command line. `tak.toml` can hold `deny = []`, and an environment variable
/// can be set to the empty string, because for those two the presence of the
/// key is itself the signal.
pub fn from_cli(v: Vec<String>) -> Option<Vec<String>> {
    (!v.is_empty()).then_some(v)
}

/// How the process environment is read.
///
/// Injected rather than called directly so tests can exercise the declared
/// variables without mutating the environment of the whole test binary, which
/// races every other test in it.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Read a boolean setting from the environment.
///
/// The usual spellings, because someone writing `TAK_CREDIT=0` in a workflow
/// should not have to discover that only `false` counts. Anything unrecognised
/// warns rather than silently meaning one of them.
fn bool_from_env(env: EnvLookup, key: &str) -> Option<bool> {
    let raw = env(key)?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => {
            eprintln!("warning: {key} is not a boolean: {raw:?}");
            None
        }
    }
}

/// Read a list-valued setting from the environment.
///
/// Presence is the signal: `TAK_ENV_DENY=` yields an empty list rather than
/// falling through to `tak.toml`, because someone who exported the variable
/// meant to say something.
fn list_from_env(env: EnvLookup, key: &str) -> Option<Vec<String>> {
    env(key).map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
}

impl Settings {
    /// Resolve every setting from the sources declared in `settings.toml`.
    pub fn resolve(cli: &Overrides, config: &SettingsSections, env: EnvLookup) -> Self {
        let defaults = Self::default();
        let envs = config.env.as_ref();
        Self {
            credit: cli
                .credit
                .or_else(|| bool_from_env(env, "TAK_CREDIT"))
                .or_else(|| config.report.as_ref().and_then(|r| r.credit))
                .unwrap_or(defaults.credit),
            gate_pct: cli
                .gate_pct
                .or_else(|| {
                    env("TAK_GATE_PCT").and_then(|raw| match raw.trim().parse::<f64>() {
                        Ok(v) => Some(v),
                        // A typo must not silently become the default and let a
                        // regression through a gate the user thought they set.
                        Err(_) => {
                            eprintln!("warning: TAK_GATE_PCT is not a number: {raw:?}");
                            None
                        }
                    })
                })
                .or_else(|| config.gate.as_ref().and_then(|g| g.pct))
                .unwrap_or(defaults.gate_pct),
            env_allow: cli
                .env_allow
                .clone()
                .or_else(|| list_from_env(env, "TAK_ENV_ALLOW"))
                .or_else(|| envs.and_then(|c| c.allow.clone()))
                .unwrap_or(defaults.env_allow),
            env_deny: cli
                .env_deny
                .clone()
                .or_else(|| list_from_env(env, "TAK_ENV_DENY"))
                .or_else(|| envs.and_then(|c| c.deny.clone()))
                .unwrap_or(defaults.env_deny),
        }
    }

    /// Resolve against the real process environment.
    pub fn from_process(cli: &Overrides, config: &SettingsSections) -> Self {
        Self::resolve(cli, config, &|key| std::env::var(key).ok())
    }

    /// The value of a setting, by its registry name.
    ///
    /// Exists so display code cannot silently omit a setting: `SETTINGS` is
    /// generated, so a new entry appears in `tak settings` whether or not
    /// anything can produce its value. A test asserts this returns `Some` for
    /// every registry entry, which turns "added a setting, forgot the
    /// accessor" into a build failure instead of a blank row.
    pub fn display_value(&self, name: &str) -> Option<String> {
        match name {
            "env_allow" => Some(format!("{:?}", self.env_allow)),
            "env_deny" => Some(format!("{:?}", self.env_deny)),
            "credit" => Some(format!("{}", self.credit)),
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

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// A `tak.toml` with just an `[env]` table.
    fn env_config(deny: Option<&[&str]>, allow: Option<&[&str]>) -> SettingsSections {
        SettingsSections {
            env: Some(crate::config::EnvSection {
                deny: deny.map(|v| v.iter().map(|s| s.to_string()).collect()),
                allow: allow.map(|v| v.iter().map(|s| s.to_string()).collect()),
            }),
            gate: None,
            report: None,
        }
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
            gate_pct: Settings::default().gate_pct,
            credit: Settings::default().credit,
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
            gate_pct: Settings::default().gate_pct,
            credit: Settings::default().credit,
        };
        assert_eq!(s.scrubbed_env().collect::<Vec<_>>(), ["A"]);
    }

    #[test]
    fn cli_beats_env_beats_config() {
        let cfg = env_config(Some(&["FROM_CONFIG"]), None);
        let env = |k: &str| (k == "TAK_ENV_DENY").then(|| "FROM_ENV".to_string());

        let from_config = Settings::resolve(&Overrides::default(), &cfg, &no_env);
        assert_eq!(from_config.env_deny, ["FROM_CONFIG"]);

        let from_env = Settings::resolve(&Overrides::default(), &cfg, &env);
        assert_eq!(from_env.env_deny, ["FROM_ENV"]);

        let cli = Overrides {
            env_deny: Some(vec!["FROM_CLI".into()]),
            ..Default::default()
        };
        let from_cli = Settings::resolve(&cli, &cfg, &env);
        assert_eq!(from_cli.env_deny, ["FROM_CLI"]);
    }

    #[test]
    fn an_absent_source_defers_rather_than_clearing() {
        let cfg = env_config(Some(&["FROM_CONFIG"]), None);
        // No CLI flag and no variable: the config value survives.
        let s = Settings::resolve(&Overrides::default(), &cfg, &no_env);
        assert_eq!(s.env_deny, ["FROM_CONFIG"]);
    }

    /// An exported-but-empty variable is a deliberate empty list. Falling
    /// through to `tak.toml` here would make `TAK_ENV_DENY=` silently do the
    /// opposite of what it looks like.
    #[test]
    fn an_empty_variable_means_an_empty_list() {
        let cfg = env_config(Some(&["FROM_CONFIG"]), None);
        let env = |k: &str| (k == "TAK_ENV_DENY").then(String::new);
        let s = Settings::resolve(&Overrides::default(), &cfg, &env);
        assert!(s.env_deny.is_empty());
    }

    #[test]
    fn a_variable_is_split_on_commas_and_trimmed() {
        let env = |k: &str| (k == "TAK_ENV_DENY").then(|| " A , B ,, C ".to_string());
        let s = Settings::resolve(&Overrides::default(), &SettingsSections::default(), &env);
        assert_eq!(s.env_deny, ["A", "B", "C"]);
    }

    #[test]
    fn an_unused_cli_flag_is_not_an_empty_list() {
        assert_eq!(from_cli(vec![]), None);
        assert_eq!(from_cli(vec!["A".into()]), Some(vec!["A".to_string()]));
    }

    /// The drift guard. A setting added to `settings.toml` gets a field and a
    /// row in `tak settings` for free, but nothing forces it to be *wired* into
    /// `resolve`. This asserts every declared environment variable actually
    /// changes the resolved settings, so adding one and forgetting the wiring
    /// fails here rather than shipping a setting that reads as supported.
    /// A value that differs from every default and parses as every supported
    /// type: a list sees `["12345"]`, a float sees `12345`. Using a word here
    /// would make the float settings silently fall through to their default and
    /// the drift check would pass while proving nothing.
    const ENV_SENTINEL: &str = "12345";

    /// Booleans need their own: `12345` is not one, and the drift check would
    /// pass while proving the setting was never wired.
    const BOOL_ENV_SENTINEL: &str = "false";

    /// The TOML literal for a sentinel of this registry type.
    fn config_sentinel(type_: &str) -> String {
        match type_ {
            "list<string>" => "[\"SENTINEL\"]".to_string(),
            "float" => "12345.0".to_string(),
            // The opposite of every bool default, so flipping it always shows.
            "bool" => "false".to_string(),
            other => panic!("the drift check has no sentinel for type `{other}`"),
        }
    }

    #[test]
    fn every_declared_env_var_is_honoured() {
        for setting in SETTINGS {
            for var in setting.env_vars {
                let sentinel = if setting.type_ == "bool" {
                    BOOL_ENV_SENTINEL
                } else {
                    ENV_SENTINEL
                };
                let env = |k: &str| (k == *var).then(|| sentinel.to_string());
                let got =
                    Settings::resolve(&Overrides::default(), &SettingsSections::default(), &env);
                assert_ne!(
                    got,
                    Settings::default(),
                    "`{}` declares {var} but setting it changes nothing — \
                     is it wired into Settings::resolve?",
                    setting.name
                );
            }
        }
    }

    /// The same guard for `tak.toml`. A dotted registry key is valid TOML on
    /// its own, so this builds the smallest config that sets exactly that key
    /// and checks it lands — which also proves the key spelled in the registry
    /// is the one `Config` actually deserializes.
    #[test]
    fn every_declared_config_key_is_honoured() {
        for setting in SETTINGS {
            for key in setting.config_keys {
                let text = format!("{key} = {}\n", config_sentinel(setting.type_));
                let cfg: SettingsSections = toml::from_str(&text).unwrap_or_else(|e| {
                    panic!(
                        "`{}` declares config key `{key}`, which does not parse: {e}",
                        setting.name
                    )
                });
                let got = Settings::resolve(&Overrides::default(), &cfg, &no_env);
                assert_ne!(
                    got,
                    Settings::default(),
                    "`{}` declares config key `{key}` but setting it changes nothing",
                    setting.name
                );
            }
        }
    }

    /// Display code reads values by registry name, and `SETTINGS` is generated,
    /// so a new setting shows up in `tak settings` whether or not its value can
    /// be produced. This is what stops that being a blank row.
    #[test]
    fn every_setting_has_an_accessor() {
        let s = Settings::default();
        for setting in SETTINGS {
            assert!(
                s.display_value(setting.name).is_some(),
                "`{}` has no accessor in Settings::get",
                setting.name
            );
        }
    }

    /// Every setting must be reachable somehow, or it is documentation for a
    /// feature that does not exist.
    #[test]
    fn every_setting_declares_a_source() {
        for s in SETTINGS {
            assert!(
                !s.cli_flags.is_empty() || !s.env_vars.is_empty() || !s.config_keys.is_empty(),
                "`{}` has no sources",
                s.name
            );
        }
    }
}
