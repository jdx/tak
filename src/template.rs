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

/// Check a value's template compiles, without rendering it. Run over the
/// whole of `tak.toml` when it is loaded, so a typo fails before any
/// benchmark runs even when the variables it needs are only set later.
pub fn check_str(value: &str) -> Result<()> {
    if is_template(value) {
        Tera::default()
            .add_raw_template("check", value)
            .with_context(|| format!("invalid template: {value:?}"))?;
    }
    Ok(())
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

    // `vars` may refer to each other, in any order: render in passes, each
    // with the values finished so far, until a pass makes no progress. What
    // is left then refers to something undefined, or to itself.
    let mut done: BTreeMap<String, String> = BTreeMap::new();
    let mut pending: Vec<(&String, &String)> = s.vars.iter().collect();
    while !pending.is_empty() {
        ctx.insert("vars", &done);
        let before = pending.len();
        let mut failed = None;
        pending.retain(|&(k, v)| match one(&ctx, &format!("vars.{k}"), v) {
            Ok(r) => {
                done.insert(k.clone(), r);
                false
            }
            Err(e) => {
                failed.get_or_insert(e);
                true
            }
        });
        if pending.len() == before {
            return Err(failed.expect("a pass without progress had a failure"));
        }
    }
    ctx.insert("vars", &done);
    s.vars = done;

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

    /// `vars` can build on each other, whatever order they are written in.
    #[test]
    fn vars_may_refer_to_other_vars() {
        let s = subject(
            "[bench.b]\ncmd = [\"{{ vars.project }}\"]\nvars = { project = \"{{ vars.root }}/app\", root = \"/tmp\" }",
        );
        let r = render(s, "b", &env(&[])).unwrap();
        assert_eq!(r.cmd, ["/tmp/app"]);
    }

    #[test]
    fn a_var_cycle_is_an_error_not_a_hang() {
        let s = subject(
            "[bench.b]\ncmd = [\"x\"]\nvars = { a = \"{{ vars.b }}\", b = \"{{ vars.a }}\" }",
        );
        assert!(render(s, "b", &env(&[])).is_err());
    }

    /// A template in a string command stays one argument rather than being
    /// split at the spaces inside its tag.
    #[test]
    fn a_string_command_keeps_template_tags_whole() {
        let s = subject(
            "[bench.b]\ncmd = \"mycli --lockfile {{ vars.lockfile }}\"\nvars = { lockfile = \"a.lock\" }",
        );
        assert_eq!(s.cmd, ["mycli", "--lockfile", "{{ vars.lockfile }}"]);
        let r = render(s, "b", &env(&[])).unwrap();
        assert_eq!(r.cmd, ["mycli", "--lockfile", "a.lock"]);
    }
}
