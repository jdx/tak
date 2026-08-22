//! Every CLI flag in the registry must actually reach the resolved settings.
//!
//! The unit tests in `settings.rs` cover the environment and `tak.toml`, but
//! not the flags: clap builds those in `main.rs`, which a library test cannot
//! see. Codegen makes it cheap to declare a source and forget to wire it, and a
//! setting that advertises `--env-deny` while ignoring it is worse than one
//! that never claimed to have a flag.
//!
//! So this drives the real binary. `tak settings` prints each resolved value,
//! which makes the check simple: pass the flag, look for the value.

use std::process::Command;

/// The value passed to every flag.
///
/// Numeric, and deliberately so: the registry holds both list and float
/// settings, and a word would be rejected outright by clap on a float flag. A
/// number is a legal value for every supported type, so one sentinel covers
/// them all — a list renders it as `["918273"]`, a float as `918273`. It is
/// implausible enough not to appear in the output by chance, so finding it
/// means the flag reached the resolver.
const SENTINEL: &str = "918273";

fn settings_output(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_tak"))
        .arg("settings")
        .args(args)
        // A stray tak.toml or exported variable in the test environment would
        // otherwise be able to mask a missing flag.
        .env_remove("TAK_ENV_DENY")
        .env_remove("TAK_ENV_ALLOW")
        .env_remove("TAK_GATE_PCT")
        .env_remove("TAK_CREDIT")
        .current_dir(std::env::temp_dir())
        .output()
        .expect("failed to run tak settings");
    assert!(
        out.status.success(),
        "tak settings failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn every_declared_cli_flag_reaches_the_resolver() {
    let baseline = settings_output(&[]);
    for setting in tak_cli::settings::Settings::SETTINGS_PROPS {
        for flag in setting.cli {
            // A boolean flag takes no value, so there is no sentinel to look
            // for — the proof is that passing it changes the listing at all.
            if setting.ty == usage_rs::config::Ty::Bool {
                let output = settings_output(&[flag]);
                assert_ne!(
                    output, baseline,
                    "`{}` declares {flag} but passing it changed nothing",
                    setting.key
                );
                continue;
            }
            let output = settings_output(&[flag, SENTINEL]);
            assert!(
                output.contains(SENTINEL),
                "`{}` declares {flag} but passing it changed nothing.\n{output}",
                setting.key
            );
        }
    }
}

/// Every setting must appear in the listing. `SETTINGS_PROPS` is generated, so
/// this fails when a new entry has no accessor rather than printing a blank row.
#[test]
fn every_setting_appears_in_the_listing() {
    let output = settings_output(&[]);
    for setting in tak_cli::settings::Settings::SETTINGS_PROPS {
        assert!(
            output.contains(setting.key),
            "`{}` is missing from `tak settings`.\n{output}",
            setting.key
        );
    }
    assert!(
        !output.contains("no accessor"),
        "a setting fell through to the no-accessor branch.\n{output}"
    );
}

/// Make a scratch directory holding exactly this `tak.toml`.
fn dir_with_config(tag: &str, contents: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tak-cfg-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tak.toml"), contents).unwrap();
    dir
}

fn run_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tak"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run tak")
}

/// An explicit command has never depended on the declared benchmarks, and
/// resolving settings must not quietly change that. A `[bench]` entry tak would
/// reject is irrelevant to `tak run -- somecmd`.
#[cfg(unix)]
#[test]
fn an_invalid_benchmark_does_not_block_an_ad_hoc_run() {
    let dir = dir_with_config("badbench", "[bench.broken]\ncmd = []\n");
    let out = run_in(
        &dir,
        &[
            "run",
            "--no-counters",
            "--runs",
            "1",
            "--warmup",
            "0",
            "--",
            "/bin/echo",
            "hi",
        ],
    );
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        out.status.success(),
        "an unrelated broken benchmark blocked an explicit run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Same reasoning for the settings listing: it reads `[env]`, not `[bench]`.
#[test]
fn an_invalid_benchmark_does_not_block_the_listing() {
    let dir = dir_with_config("badbench2", "[bench.broken]\ncmd = []\n");
    let out = run_in(&dir, &["settings"]);
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        out.status.success(),
        "a broken benchmark blocked `tak settings`: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Commands that measure nothing do not read `tak.toml` at all, so a file that
/// cannot be parsed must not stand between you and pushing results you already
/// have. `history` needs no network and no notes to exit cleanly.
#[test]
fn a_broken_config_does_not_block_commands_that_ignore_it() {
    let dir = dir_with_config("syntax", "this is not toml at all\n");
    // Not a git repository either, so this fails for git reasons if at all —
    // what matters is that the failure is never about tak.toml.
    let out = run_in(&dir, &["history"]);
    std::fs::remove_dir_all(&dir).ok();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("tak.toml"),
        "a command that ignores tak.toml complained about it: {stderr}"
    );
}

/// A well-formed `tak.toml` with the wrong value type must be reported, not silently
/// replaced by defaults —
/// otherwise `tak settings` confidently prints values the project did not ask
/// for and gives no hint that its configuration was skipped.
#[test]
fn a_wrongly_typed_config_is_an_error_not_a_default() {
    let dir = std::env::temp_dir().join(format!("tak-bad-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tak.toml"), "[env]\ndeny = \"not a list\"\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_tak"))
        .arg("settings")
        .current_dir(&dir)
        .output()
        .expect("failed to run tak settings");

    std::fs::remove_dir_all(&dir).ok();

    assert!(
        !out.status.success(),
        "a broken tak.toml should not succeed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("tak.toml"),
        "the error should name the file: {stderr}"
    );
}

/// Syntax errors take the same strict path as type errors.
#[test]
fn malformed_toml_is_an_error_not_a_default() {
    let dir = dir_with_config("malformed", "[env\nallow = [\"PATH\"]\n");
    let out = run_in(&dir, &["settings"]);
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        !out.status.success(),
        "a syntactically broken tak.toml should not succeed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("tak.toml"),
        "the error should name the file: {stderr}"
    );
}
