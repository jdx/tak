//! tak — benchmark command-line programs and track their performance over time.
//!
//! Experimental. See README.md, which asks you to use hyperfine instead.

mod measure;
mod notes;
mod record;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;

use measure::Plan;
use record::{Record, SCHEMA_VERSION};

#[derive(Parser)]
#[command(name = "tak", version, about = "CLI performance, tracked", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Benchmark a command.
    Run {
        /// Name to record this measurement under.
        #[arg(long, default_value = "default")]
        bench: String,
        /// Timed runs.
        #[arg(long, default_value_t = 20)]
        runs: u32,
        /// Untimed warmup runs.
        #[arg(long, default_value_t = 3)]
        warmup: u32,
        /// Skip instruction counting even where valgrind is available.
        #[arg(long)]
        no_counters: bool,
        /// Append the result to refs/notes/tak for the current commit.
        #[arg(long)]
        record: bool,
        /// Command to benchmark, after `--`.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Show recorded history for a commit.
    History {
        /// Revision to read. Defaults to HEAD.
        #[arg(default_value = "HEAD")]
        rev: String,
        /// Remote to refresh notes from.
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// Push recorded measurements to the remote.
    Push {
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// Teach plain `git fetch` about the notes ref.
    Init {
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// Diagnose the git-notes plumbing.
    Doctor,
}

/// Identify the machine class. Series must be partitioned on this — moving
/// between runner types shifts absolute numbers enough to look like a regression,
/// which is a documented failure mode of every threshold-based CI benchmark.
fn runner_class() -> String {
    if let Ok(v) = std::env::var("TAK_RUNNER") {
        return v;
    }
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        let os = std::env::var("RUNNER_OS").unwrap_or_else(|_| "unknown".into());
        let arch = std::env::var("RUNNER_ARCH").unwrap_or_else(|_| "unknown".into());
        return format!("gha-{}-{}", os.to_lowercase(), arch.to_lowercase());
    }
    format!("local-{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// RFC 3339 to second resolution, without pulling in a date crate for a skeleton.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm), epoch shifted to 0000-03-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn cmd_run(
    bench: String,
    runs: u32,
    warmup: u32,
    no_counters: bool,
    record_it: bool,
    cmd: Vec<String>,
) -> Result<()> {
    let plan = Plan {
        cmd: cmd.clone(),
        warmup,
        runs,
    };

    let mut metrics: BTreeMap<String, f64> = measure::wall(&plan)?;

    if !no_counters {
        match measure::instructions(&cmd)? {
            Some(n) => {
                metrics.insert("instructions".into(), n as f64);
            }
            None => eprintln!(
                "note: valgrind not found — recording timing only. \
                 Instruction counts are the only gate-able metric; on macOS/Windows \
                 run tak in a Linux container to get them."
            ),
        }
    }

    println!("  {}", cmd.join(" "));
    for (k, v) in &metrics {
        if k == "wall_n" {
            continue;
        }
        if k == "instructions" {
            println!("  {k:<16} {v:>14.0}");
        } else {
            println!("  {k:<16} {v:>14.2}");
        }
    }

    if record_it {
        let sha = notes::rev_parse("HEAD").context("not in a git repository")?;
        let rec = Record {
            v: SCHEMA_VERSION,
            bench,
            tool: std::env::var("TAK_TOOL").unwrap_or_else(|_| "self".into()),
            version: None,
            runner: runner_class(),
            ts: now_rfc3339(),
            metrics,
        };
        notes::append(&sha, &[rec])?;
        println!("\n  recorded to {} for {}", notes::NOTES_REF, &sha[..12]);
        println!("  push with: tak push");
    }

    Ok(())
}

fn cmd_history(rev: String, remote: String) -> Result<()> {
    let sha = notes::rev_parse(&rev).context("not in a git repository")?;
    let recs = notes::read(Some(&remote), &sha)?;
    if recs.is_empty() {
        println!("no measurements recorded for {}", &sha[..12]);
        return Ok(());
    }
    println!("{} measurement(s) for {}\n", recs.len(), &sha[..12]);
    for r in recs {
        let ins = r
            .metrics
            .get("instructions")
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "-".into());
        let wall = r
            .metrics
            .get("wall_min_ms")
            .map(|v| format!("{v:.2}ms"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:<16} {:<10} {:<22} instructions={:<14} wall_min={}",
            r.bench, r.tool, r.runner, ins, wall
        );
    }
    Ok(())
}

fn cmd_doctor() -> Result<()> {
    println!("tak doctor\n");

    match notes::rev_parse("HEAD") {
        Ok(sha) => println!("  ✓ git repository        HEAD {}", &sha[..12]),
        Err(_) => {
            println!("  ✗ git repository        not in one — nothing can be recorded");
            return Ok(());
        }
    }

    match std::process::Command::new("valgrind")
        .arg("--version")
        .output()
    {
        Ok(o) if o.status.success() => println!(
            "  ✓ valgrind              {}",
            String::from_utf8_lossy(&o.stdout).trim()
        ),
        _ => println!(
            "  ! valgrind              not found — timing only, no gate-able metric\n\
             \x20                         (expected on macOS/Windows; use a Linux container)"
        ),
    }

    match notes::fetch("origin") {
        Ok(true) => println!("  ✓ notes fetch           refreshed {} from origin", notes::NOTES_REF),
        Ok(false) => println!(
            "  ! notes fetch           could not fetch {} (no remote, offline, or no data yet)",
            notes::NOTES_REF
        ),
        Err(e) => println!("  ! notes fetch           {e}"),
    }

    println!("  · runner class          {}", runner_class());
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            bench,
            runs,
            warmup,
            no_counters,
            record,
            cmd,
        } => cmd_run(bench, runs, warmup, no_counters, record, cmd),
        Cmd::History { rev, remote } => cmd_history(rev, remote),
        Cmd::Push { remote } => {
            notes::push(&remote)?;
            println!("pushed {} to {remote}", notes::NOTES_REF);
            Ok(())
        }
        Cmd::Init { remote } => {
            notes::install_refspec(&remote)?;
            println!(
                "added {} to remote.{remote}.fetch — plain `git fetch` now picks up measurements",
                notes::NOTES_REF
            );
            Ok(())
        }
        Cmd::Doctor => cmd_doctor(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_rfc3339_shaped() {
        let ts = now_rfc3339();
        assert_eq!(ts.len(), 20, "{ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn runner_class_is_overridable() {
        // SAFETY: single-threaded test, no concurrent env access.
        unsafe { std::env::set_var("TAK_RUNNER", "ns-endev-linux-amd64") };
        assert_eq!(runner_class(), "ns-endev-linux-amd64");
        unsafe { std::env::remove_var("TAK_RUNNER") };
    }
}
