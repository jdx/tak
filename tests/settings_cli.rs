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

/// The value passed to every flag. Unlikely to appear in `tak settings` output
/// by chance, so finding it means the flag reached the resolver.
const SENTINEL: &str = "TAK_SENTINEL_VALUE";

fn settings_output(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_tak"))
        .arg("settings")
        .args(args)
        // A stray tak.toml or exported variable in the test environment would
        // otherwise be able to mask a missing flag.
        .env_remove("TAK_ENV_DENY")
        .env_remove("TAK_ENV_ALLOW")
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
    for setting in tak_cli::settings::SETTINGS {
        for flag in setting.cli_flags {
            let output = settings_output(&[flag, SENTINEL]);
            assert!(
                output.contains(SENTINEL),
                "`{}` declares {flag} but passing it changed nothing.\n{output}",
                setting.name
            );
        }
    }
}

/// Every setting must appear in the listing. `SETTINGS` is generated, so this
/// fails when a new entry has no accessor rather than printing a blank row.
#[test]
fn every_setting_appears_in_the_listing() {
    let output = settings_output(&[]);
    for setting in tak_cli::settings::SETTINGS {
        assert!(
            output.contains(setting.name),
            "`{}` is missing from `tak settings`.\n{output}",
            setting.name
        );
    }
    assert!(
        !output.contains("no accessor"),
        "a setting fell through to the no-accessor branch.\n{output}"
    );
}

/// A malformed `tak.toml` must be reported, not silently replaced by defaults —
/// otherwise `tak settings` confidently prints values the project did not ask
/// for and gives no hint that its configuration was skipped.
#[test]
fn a_malformed_config_is_an_error_not_a_default() {
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
