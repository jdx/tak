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

use crate::config::EnvSection;

include!(concat!(env!("OUT_DIR"), "/settings_generated.rs"));

/// Values supplied on the command line.
///
/// `None` means the flag was not given, which is not the same as being given an
/// empty list — the first defers to lower-precedence sources, the second would
/// override them. Repeated flags accumulate, and clap yields an empty vector
/// when a flag is absent, so [`from_cli`] does that conversion in one place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub env_deny: Option<Vec<String>>,
    pub env_allow: Option<Vec<String>>,
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
    pub fn resolve(cli: &Overrides, config: Option<&EnvSection>, env: EnvLookup) -> Self {
        let defaults = Self::default();
        Self {
            env_allow: cli
                .env_allow
                .clone()
                .or_else(|| list_from_env(env, "TAK_ENV_ALLOW"))
                .or_else(|| config.and_then(|c| c.allow.clone()))
                .unwrap_or(defaults.env_allow),
            env_deny: cli
                .env_deny
                .clone()
                .or_else(|| list_from_env(env, "TAK_ENV_DENY"))
                .or_else(|| config.and_then(|c| c.deny.clone()))
                .unwrap_or(defaults.env_deny),
        }
    }

    /// Resolve against the real process environment.
    pub fn from_process(cli: &Overrides, config: Option<&EnvSection>) -> Self {
        Self::resolve(cli, config, &|key| std::env::var(key).ok())
    }

    /// The value of a setting, by its registry name.
    ///
    /// Exists so display code cannot silently omit a setting: `SETTINGS` is
    /// generated, so a new entry appears in `tak settings` whether or not
    /// anything can produce its value. A test asserts this returns `Some` for
    /// every registry entry, which turns "added a setting, forgot the
    /// accessor" into a build failure instead of a blank row.
    pub fn get(&self, name: &str) -> Option<&Vec<String>> {
        match name {
            "env_allow" => Some(&self.env_allow),
            "env_deny" => Some(&self.env_deny),
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
        };
        assert_eq!(s.scrubbed_env().collect::<Vec<_>>(), ["A"]);
    }

    #[test]
    fn cli_beats_env_beats_config() {
        let cfg = EnvSection {
            deny: Some(vec!["FROM_CONFIG".into()]),
            allow: None,
        };
        let env = |k: &str| (k == "TAK_ENV_DENY").then(|| "FROM_ENV".to_string());

        let from_config = Settings::resolve(&Overrides::default(), Some(&cfg), &no_env);
        assert_eq!(from_config.env_deny, ["FROM_CONFIG"]);

        let from_env = Settings::resolve(&Overrides::default(), Some(&cfg), &env);
        assert_eq!(from_env.env_deny, ["FROM_ENV"]);

        let cli = Overrides {
            env_deny: Some(vec!["FROM_CLI".into()]),
            ..Default::default()
        };
        let from_cli = Settings::resolve(&cli, Some(&cfg), &env);
        assert_eq!(from_cli.env_deny, ["FROM_CLI"]);
    }

    #[test]
    fn an_absent_source_defers_rather_than_clearing() {
        let cfg = EnvSection {
            deny: Some(vec!["FROM_CONFIG".into()]),
            allow: None,
        };
        // No CLI flag and no variable: the config value survives.
        let s = Settings::resolve(&Overrides::default(), Some(&cfg), &no_env);
        assert_eq!(s.env_deny, ["FROM_CONFIG"]);
    }

    /// An exported-but-empty variable is a deliberate empty list. Falling
    /// through to `tak.toml` here would make `TAK_ENV_DENY=` silently do the
    /// opposite of what it looks like.
    #[test]
    fn an_empty_variable_means_an_empty_list() {
        let cfg = EnvSection {
            deny: Some(vec!["FROM_CONFIG".into()]),
            allow: None,
        };
        let env = |k: &str| (k == "TAK_ENV_DENY").then(String::new);
        let s = Settings::resolve(&Overrides::default(), Some(&cfg), &env);
        assert!(s.env_deny.is_empty());
    }

    #[test]
    fn a_variable_is_split_on_commas_and_trimmed() {
        let env = |k: &str| (k == "TAK_ENV_DENY").then(|| " A , B ,, C ".to_string());
        let s = Settings::resolve(&Overrides::default(), None, &env);
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
    #[test]
    fn every_declared_env_var_is_honoured() {
        for setting in SETTINGS {
            for var in setting.env_vars {
                let env = |k: &str| (k == *var).then(|| "SENTINEL".to_string());
                let got = Settings::resolve(&Overrides::default(), None, &env);
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
                let text = format!("{key} = [\"SENTINEL\"]\n");
                let cfg: crate::config::Config = toml::from_str(&text).unwrap_or_else(|e| {
                    panic!(
                        "`{}` declares config key `{key}`, which does not parse: {e}",
                        setting.name
                    )
                });
                let got = Settings::resolve(&Overrides::default(), cfg.env.as_ref(), &no_env);
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
                s.get(setting.name).is_some(),
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
