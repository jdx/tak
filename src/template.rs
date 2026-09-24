//! Templates in `tak.toml` values, rendered with [tera](https://keats.github.io/tera/) —
//! the same syntax mise uses, so `{{ env.HOME }}` means the same thing in both
//! files.
//!
//! tak renders the template itself; no shell is involved, which keeps a
//! subject's command a plain argv even when it needs a path that only exists
//! at run time. Rendering happens once, before anything is measured, and an
//! undefined variable is an error then rather than a command that fails — or
//! worse, runs against the wrong path — halfway through a long comparison.
//!
//! In scope for every value:
//!
//! - `env` — tak's own environment, e.g. `{{ env.BENCH_DIR }}`
//! - `bench` — the benchmark's name
//! - `subject` — the subject's name (`self` for a single-command benchmark)
//! - `vars` — the subject's `vars` tables, merged like `env`
//!
//! `vars` values are templates too, rendered first, so one can build on `env`.

use crate::config::Subject;
use anyhow::{Context as _, Result};
use std::collections::BTreeMap;
use tera::{Context, Tera};

/// tak's environment as a template value. Lossy on non-UTF-8 values, which
/// cannot be written into a TOML string anyway.
pub fn env() -> BTreeMap<String, String> {
    std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// Whether `s` contains template syntax at all. Plain values skip tera
/// entirely, so a file with no templates costs nothing.
fn is_template(s: &str) -> bool {
    s.contains("{{") || s.contains("{%") || s.contains("{#")
}

/// Check every template in `s` compiles, without rendering. Run when
/// `tak.toml` is loaded, so a typo fails before any benchmark runs even when
/// the variables it needs are only set later.
pub fn check(s: &Subject) -> Result<()> {
    let mut tera = Tera::default();
    for (field, value) in strings(s) {
        if is_template(value) {
            tera.add_raw_template("check", value)
                .with_context(|| format!("invalid template in {field}: {value:?}"))?;
        }
    }
    Ok(())
}

/// Every template-bearing string of a subject, with a name for errors.
fn strings(s: &Subject) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    out.extend(s.cmd.iter().map(|a| ("cmd".to_string(), a.as_str())));
    for p in s.prepare.iter().flatten() {
        out.push(("prepare".to_string(), p.as_str()));
    }
    out.extend(s.env.iter().map(|(k, v)| (format!("env.{k}"), v.as_str())));
    if let Some(d) = s.dir.as_ref().and_then(|d| d.to_str()) {
        out.push(("dir".to_string(), d));
    }
    out.extend(
        s.vars
            .iter()
            .map(|(k, v)| (format!("vars.{k}"), v.as_str())),
    );
    out
}

/// Render every template in `s` for benchmark `bench`.
pub fn render(mut s: Subject, bench: &str, env: &BTreeMap<String, String>) -> Result<Subject> {
    let tera = Tera::default();
    let mut ctx = Context::new();
    ctx.insert("env", env);
    ctx.insert("bench", bench);
    ctx.insert("subject", &s.name);

    let one = |ctx: &Context, field: &str, value: &str| -> Result<String> {
        if !is_template(value) {
            return Ok(value.to_string());
        }
        tera.render_str(value, ctx, false)
            .map_err(|e| anyhow::anyhow!("{e:#}"))
            .with_context(|| format!("could not render {field}: {value:?}"))
    };

    let vars: BTreeMap<String, String> = s
        .vars
        .iter()
        .map(|(k, v)| Ok((k.clone(), one(&ctx, &format!("vars.{k}"), v)?)))
        .collect::<Result<_>>()?;
    ctx.insert("vars", &vars);
    s.vars = vars;

    for a in &mut s.cmd {
        *a = one(&ctx, "cmd", a)?;
    }
    if let Some(prepare) = &mut s.prepare {
        for a in prepare {
            *a = one(&ctx, "prepare", a)?;
        }
    }
    if let Some(dir) = &mut s.dir {
        let text = dir.to_string_lossy().into_owned();
        *dir = one(&ctx, "dir", &text)?.into();
    }
    for (k, v) in &mut s.env {
        *v = one(&ctx, &format!("env.{k}"), v)?;
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn subject(toml: &str) -> Subject {
        Config::parse(toml)
            .unwrap()
            .subjects("b")
            .unwrap()
            .remove(0)
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn values_are_rendered_from_env_bench_subject_and_vars() {
        let s = subject(
            r#"
            [subject.aube]
            cmd = ["{{ env.AUBE_BIN }}", "install", "--lockfile={{ vars.lockfile }}"]
            prepare = ["cp", "{{ vars.saved }}", "."]
            dir = "{{ env.BENCH_DIR }}/project-{{ subject }}"
            env = { HOME = "{{ env.BENCH_DIR }}/home-{{ subject }}", TAG = "{{ bench }}" }
            vars = { lockfile = "aube-lock.yaml", saved = "{{ env.BENCH_DIR }}/saved-{{ subject }}" }

            [bench.b]
            subjects = ["aube"]
            "#,
        );
        let r = render(
            s,
            "b",
            &env(&[("AUBE_BIN", "/bin/aube"), ("BENCH_DIR", "/tmp/x")]),
        )
        .unwrap();
        assert_eq!(r.cmd, ["/bin/aube", "install", "--lockfile=aube-lock.yaml"]);
        assert_eq!(r.prepare.unwrap(), ["cp", "/tmp/x/saved-aube", "."]);
        assert_eq!(r.dir.unwrap(), std::path::Path::new("/tmp/x/project-aube"));
        assert_eq!(r.env["HOME"], "/tmp/x/home-aube");
        assert_eq!(r.env["TAG"], "b");
    }

    /// An unset variable is an error before anything runs, not an empty
    /// string that sends a command to the wrong path.
    #[test]
    fn an_undefined_variable_is_an_error() {
        let s = subject("[bench.b]\ncmd = [\"{{ env.NOT_SET_ANYWHERE }}\"]");
        let err = render(s, "b", &env(&[])).unwrap_err();
        assert!(format!("{err:#}").contains("cmd"), "{err:#}");
    }

    #[test]
    fn filters_work_and_plain_values_pass_through() {
        let s = subject(
            "[bench.b]\ncmd = [\"{{ env.X | default(value='fallback') }}\", \"plain $HOME\"]",
        );
        let r = render(s, "b", &env(&[])).unwrap();
        assert_eq!(r.cmd, ["fallback", "plain $HOME"]);
    }
}
