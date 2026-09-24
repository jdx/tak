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

/// The names a template refers to as `vars.NAME` or `vars["NAME"]`, inside
/// `{{ }}` and `{% %}` tags only: text outside a tag, or in a `{# #}`
/// comment, is literal and never waits on anything. A plain scan rather than
/// a parse: it only decides rendering order, and naming a var that does not
/// exist just means nothing to wait for.
fn var_refs(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    loop {
        let open = [("{{", "}}"), ("{%", "%}"), ("{#", "#}")]
            .iter()
            .filter_map(|&(o, c)| rest.find(o).map(|i| (i, o, c)))
            .min_by_key(|&(i, ..)| i);
        let Some((i, o, c)) = open else { break };
        let inner = &rest[i + o.len()..];
        let end = inner.find(c).unwrap_or(inner.len());
        if o != "{#" {
            refs_in(&inner[..end], &mut out);
        }
        rest = &inner[(end + c.len()).min(inner.len())..];
    }
    out
}

/// `vars.NAME` and `vars["NAME"]` within one tag's contents.
fn refs_in(code: &str, out: &mut Vec<String>) {
    let mut rest = code;
    while let Some(i) = rest.find("vars") {
        let after = &rest[i + 4..];
        let before_ok = rest[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if before_ok {
            if let Some(tail) = after.strip_prefix('.') {
                let name: String = tail
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    out.push(name);
                }
            } else if let Some(tail) = after.strip_prefix('[') {
                let tail = tail.trim_start();
                if let Some(q) = tail.chars().next().filter(|c| *c == '"' || *c == '\'')
                    && let Some(end) = tail[1..].find(q)
                {
                    out.push(tail[1..1 + end].to_string());
                }
            }
        }
        rest = after;
    }
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

    // `vars` may refer to each other, in any order. A var is rendered only
    // once every declared var it names is finished — not merely once it
    // renders, because a filter like `default` renders happily without the
    // var it is waiting for and would lock in its fallback. Whatever cannot
    // become ready refers to itself, directly or around a cycle.
    let mut done: BTreeMap<String, String> = BTreeMap::new();
    let mut pending: Vec<(&String, &String)> = s.vars.iter().collect();
    while !pending.is_empty() {
        let ready = pending.iter().position(|(_, v)| {
            var_refs(v)
                .iter()
                .all(|r| !s.vars.contains_key(r) || done.contains_key(r))
        });
        let Some(i) = ready else {
            let names: Vec<&str> = pending.iter().map(|(k, _)| k.as_str()).collect();
            anyhow::bail!("vars refer to each other in a cycle: {}", names.join(", "));
        };
        let (k, v) = pending.remove(i);
        ctx.insert("vars", &done);
        let rendered = one(&ctx, &format!("vars.{k}"), v)?;
        done.insert(k.clone(), rendered);
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
    if let Some(setup) = &mut s.setup {
        for a in setup {
            *a = one(&ctx, "setup", a)?;
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
            setup = ["./clone", "{{ env.BENCH_DIR }}/project-{{ subject }}"]
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
        assert_eq!(r.setup.unwrap(), ["./clone", "/tmp/x/project-aube"]);
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

    /// A fallback does not win over a var that exists but is written later
    /// in key order: rendering waits for what a var names.
    #[test]
    fn a_default_waits_for_the_var_it_names() {
        let s = subject(
            "[bench.b]\ncmd = [\"{{ vars.a }}\"]\nvars = { a = \"{{ vars.b | default(value='fallback') }}\", b = \"real\" }",
        );
        assert_eq!(render(s, "b", &env(&[])).unwrap().cmd, ["real"]);
    }

    #[test]
    fn var_references_are_found_in_both_spellings() {
        assert_eq!(
            var_refs("{{ vars.a }}-{{ vars['b'] }}-{{ myvars.c }}"),
            ["a", "b"]
        );
    }
}
