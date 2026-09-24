//! `when` conditions in `tak.toml`, written in [expr](https://expr-lang.org)
//! — the language mise uses for its own conditions — and evaluated with
//! [expr-lang](https://github.com/jdx/expr.rs).
//!
//! A benchmark or subject whose `when` is false is left out of the run
//! before anything about it is rendered, so a subject for a tool that is not
//! installed does not need the variables naming it either.
//!
//! In scope:
//!
//! - `env` — tak's environment, as a map: `env.VLT_BIN != ""`
//! - `os`, `arch` — as Rust names them: `"linux"`, `"macos"`, `"x86_64"`, `"aarch64"`
//! - `ci` — whether `CI` is set to anything but `""` or `"false"`
//! - `bench`, `subject` — the names being decided on (`subject` is empty for
//!   a benchmark's own `when`)
//!
//! Built without expr-lang's optional features: the operators, `??`, `?:`,
//! and the string and collection builtins are there; `matches`, JSON and the
//! date functions are not.

use anyhow::{Context as _, Result, bail};
use expr::{Context, Value};
use indexmap::IndexMap;
use std::collections::BTreeMap;

/// Check a condition parses, without evaluating it. Run when `tak.toml` is
/// loaded, so a typo fails before any benchmark runs.
pub fn check(when: &str) -> Result<()> {
    expr::compile(when)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("invalid `when`: {when:?}"))
}

/// Evaluate a condition for `bench` (and `subject`, if deciding on one).
pub fn eval(
    when: &str,
    env: &BTreeMap<String, String>,
    bench: &str,
    subject: Option<&str>,
) -> Result<bool> {
    let mut ctx = Context::default();
    let map: IndexMap<String, Value> = env
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
        .collect();
    ctx.insert("env", map);
    ctx.insert("os", std::env::consts::OS);
    ctx.insert("arch", std::env::consts::ARCH);
    let ci = env.get("CI").is_some_and(|v| !v.is_empty() && v != "false");
    ctx.insert("ci", ci);
    ctx.insert("bench", bench);
    ctx.insert("subject", subject.unwrap_or(""));
    let value = expr::eval(when, &ctx)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("could not evaluate `when`: {when:?}"))?;
    match value.as_bool() {
        Some(b) => Ok(b),
        None => bail!("`when` must be true or false, but {when:?} gave {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn conditions_see_env_platform_ci_and_names() {
        let e = env(&[("VLT_BIN", "/bin/vlt"), ("CI", "true")]);
        assert!(eval(r#"env.VLT_BIN != """#, &e, "b", None).unwrap());
        assert!(!eval(r#"(env.MISSING ?? "") != """#, &e, "b", None).unwrap());
        assert!(eval("ci", &e, "b", None).unwrap());
        assert!(!eval("ci", &env(&[("CI", "false")]), "b", None).unwrap());
        assert!(eval(&format!("os == {:?}", std::env::consts::OS), &e, "b", None).unwrap());
        assert!(eval(r#"bench == "b" && subject == "vlt""#, &e, "b", Some("vlt")).unwrap());
    }

    #[test]
    fn a_non_boolean_result_is_an_error() {
        let err = eval(r#""yes""#, &env(&[]), "b", None).unwrap_err();
        assert!(format!("{err:#}").contains("true or false"), "{err:#}");
    }

    #[test]
    fn a_syntax_error_is_caught_by_check() {
        assert!(check("env.X ==").is_err());
        assert!(check(r#"env.X == "y""#).is_ok());
    }

    /// `??` binds more loosely than `!=`, so the fallback needs parentheses;
    /// this pins the form the docs recommend.
    #[test]
    fn a_missing_variable_fallback_needs_parentheses() {
        let when = r#"(env.X ?? "") != """#;
        assert!(eval(when, &env(&[("X", "/bin/x")]), "b", None).unwrap());
        assert!(!eval(when, &env(&[]), "b", None).unwrap());
        assert!(eval(r#"env.X ?? "" != """#, &env(&[("X", "/bin/x")]), "b", None).is_err());
    }
}
