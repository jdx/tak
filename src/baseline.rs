//! Named local baselines, for the edit-measure loop.
//!
//! `tak compare` needs both sides recorded against commits. Someone optimising
//! a hot path wants to measure, edit, and measure again, and the edited working
//! tree has no commit to hang a note on. Recording against HEAD anyway would
//! attach a measurement to a commit whose code it does not describe, and that
//! note would travel with the next `tak push`. So a baseline is a named file of
//! records instead, compared with the same [`crate::compare`] the gate uses.
//!
//! Files live under the git *common* directory, `<git-common-dir>/tak/baselines`:
//!
//! - never committed and never pushed, since nothing under `.git` is;
//! - outside the working tree, so no `.gitignore` entry is needed and a
//!   `git clean -fdx` between edits does not delete the baseline;
//! - shared by every worktree of a clone, so a baseline saved on one branch's
//!   worktree can be compared from another's.
//!
//! One [`Record::to_line`] per line, the same bytes a note would hold, so a
//! baseline is read by the same parser and skips the same unreadable lines.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::compare::Key;
use crate::record::{Record, parse_note};

/// Extension of a baseline file. Listing only these keeps the temporary file
/// an interrupted save leaves behind out of "saved baselines".
const EXT: &str = "jsonl";

/// A saved baseline, loaded.
#[derive(Debug)]
pub struct Baseline {
    pub name: String,
    pub path: PathBuf,
    pub records: Vec<Record>,
}

/// The directory baselines are kept in.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

/// Reject a name that is not a plain file name.
///
/// The name becomes a path component, so `../x` or `a/b` would write outside
/// the store, and a leading `.` would hide the file from the listing a missing
/// baseline's error prints. A narrow character set is easier to hold than a
/// list of what to escape on every platform.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !ok {
        bail!(
            "invalid baseline name `{name}`: use letters, digits, `-`, `_` and `.`, \
             not starting with `.`"
        );
    }
    Ok(())
}

fn key(r: &Record) -> Key {
    (r.bench.clone(), r.tool.clone(), r.runner.clone())
}

impl Store {
    /// The store for the repository the current directory is in.
    ///
    /// No fallback outside a repository. Anywhere else — a per-user data
    /// directory, the current directory — is either shared between unrelated
    /// projects, where one project's `before` silently answers for another's,
    /// or inside a tree that may be committed.
    pub fn locate() -> Result<Self> {
        let out = Command::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .output()
            .context("failed to run `git rev-parse --git-common-dir`")?;
        if !out.status.success() {
            bail!(
                "baselines are kept in the repository's git directory, so they need a git \
                 repository: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end());
        // Relative to the current directory when git prints it that way
        // (`.git` at the top of a checkout), absolute from inside a worktree.
        // Anchored here so every path printed later is one the user can open.
        let common = std::path::absolute(&common)
            .with_context(|| format!("could not resolve {}", common.display()))?;
        Ok(Self::at(common.join("tak").join("baselines")))
    }

    /// A store rooted at `dir`, which need not exist yet.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Store { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.{EXT}"))
    }

    /// Names of the saved baselines, sorted.
    pub fn list(&self) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("could not read {}", self.dir.display()));
            }
        };
        let mut names = BTreeSet::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == EXT)
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && validate_name(stem).is_ok()
            {
                names.insert(stem.to_string());
            }
        }
        Ok(names.into_iter().collect())
    }

    /// Load a baseline, failing when it does not exist or holds nothing.
    ///
    /// Called before anything is measured: a mistyped name should cost a
    /// second, not a full run followed by an error.
    pub fn load(&self, name: &str) -> Result<Baseline> {
        validate_name(name)?;
        let path = self.path(name);
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let saved = self.list()?;
                if saved.is_empty() {
                    bail!(
                        "no baseline `{name}`: none saved yet. Save one first with \
                         `tak run --save-baseline {name}`"
                    );
                }
                bail!("no baseline `{name}` (saved: {})", saved.join(", "));
            }
            Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
        };
        let records = parse_note(&body);
        // Empty is an error rather than an empty comparison: the user named
        // this baseline to compare against, and a report that compared nothing
        // is easy to misread as one that found nothing.
        if records.is_empty() {
            bail!(
                "baseline `{name}` holds no measurement this version of tak can read ({})",
                path.display()
            );
        }
        Ok(Baseline {
            name: name.to_string(),
            path,
            records,
        })
    }

    /// Save `records` under `name`, returning the file written.
    ///
    /// Replaces what the baseline held for each series in `records` and keeps
    /// every other series, so `tak run --bench a --save-baseline x` updates `a`
    /// without discarding the `b` a full run saved earlier. The same series key
    /// `compare` uses, runner included: a baseline saved with `--runner` on two
    /// classes keeps one series for each.
    ///
    /// Lines this version cannot read — a newer schema, a hand edit — are kept
    /// as they are. They cannot be matched to a series, and dropping them would
    /// make an older tak destroy what a newer one saved.
    ///
    /// Written to a temporary file and renamed into place, so an interrupted
    /// save leaves the previous baseline whole rather than half of either.
    pub fn save(&self, name: &str, records: &[Record]) -> Result<PathBuf> {
        validate_name(name)?;
        let path = self.path(name);
        let existing = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
        };
        let replaced: BTreeSet<Key> = records.iter().map(key).collect();
        // Sorted and deduplicated, as a note is, so the file's bytes depend
        // only on what it holds and not on the order runs were saved in.
        let mut lines = BTreeSet::new();
        for line in existing.lines().filter(|l| !l.trim().is_empty()) {
            match Record::from_line(line) {
                Ok(Some(r)) if replaced.contains(&key(&r)) => {}
                _ => {
                    lines.insert(line.to_string());
                }
            }
        }
        for r in records {
            lines.insert(r.to_line()?);
        }

        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("could not create {}", self.dir.display()))?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir).with_context(|| {
            format!(
                "could not create a temporary file in {}",
                self.dir.display()
            )
        })?;
        for line in &lines {
            writeln!(temp, "{line}")?;
        }
        temp.as_file().sync_all()?;
        temp.persist(&path)
            .map_err(|e| e.error)
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(path)
    }
}

/// The part of a baseline this run can be compared against: the records for
/// benchmarks and tools the run measured, on any runner.
///
/// A baseline outlives the run that saved it, so `tak run --bench a` against
/// one saved by a full run would otherwise list every other benchmark as
/// "measured on the base but not here". That warning exists for a CI job that
/// silently stopped running something; here it would only be noise.
///
/// The runner is deliberately not part of the filter. A baseline saved on
/// another runner class has to stay visible, so the report names both classes
/// and says nothing was compared, instead of showing an empty table.
pub fn relevant(baseline: &[Record], current: &[Record]) -> Vec<Record> {
    let measured: BTreeSet<(&str, &str)> = current
        .iter()
        .map(|r| (r.bench.as_str(), r.tool.as_str()))
        .collect();
    baseline
        .iter()
        .filter(|r| measured.contains(&(r.bench.as_str(), r.tool.as_str())))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::SCHEMA_VERSION;
    use std::collections::BTreeMap;

    fn rec(bench: &str, runner: &str, ins: f64) -> Record {
        Record {
            v: SCHEMA_VERSION,
            bench: bench.into(),
            tool: "self".into(),
            version: None,
            runner: runner.into(),
            ts: "2026-09-29T00:00:00Z".into(),
            metrics: BTreeMap::from([("instructions".to_string(), ins)]),
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("baselines"));
        (dir, store)
    }

    fn values(b: &Baseline) -> Vec<(String, String, f64)> {
        b.records
            .iter()
            .map(|r| (r.bench.clone(), r.runner.clone(), r.metrics["instructions"]))
            .collect()
    }

    #[test]
    fn names_that_are_not_plain_file_names_are_rejected() {
        for bad in ["", ".hidden", "../up", "a/b", "a\\b", "with space", "é"] {
            assert!(validate_name(bad).is_err(), "{bad:?} was accepted");
        }
        for good in ["before", "main-2026.09.29", "v1.2_rc"] {
            validate_name(good).unwrap();
        }
    }

    #[test]
    fn a_saved_baseline_loads_back() {
        let (_tmp, s) = store();
        s.save("before", &[rec("a", "r", 10.0)]).unwrap();
        let b = s.load("before").unwrap();
        assert_eq!(values(&b), vec![("a".into(), "r".into(), 10.0)]);
        assert_eq!(s.list().unwrap(), vec!["before".to_string()]);
    }

    /// Saving a subset updates that subset and keeps the rest, the way a
    /// criterion baseline does per benchmark.
    #[test]
    fn saving_replaces_only_the_series_it_measured() {
        let (_tmp, s) = store();
        s.save("x", &[rec("a", "r", 10.0), rec("b", "r", 20.0)])
            .unwrap();
        s.save("x", &[rec("a", "r", 11.0)]).unwrap();
        let b = s.load("x").unwrap();
        assert_eq!(
            values(&b),
            vec![
                ("a".into(), "r".into(), 11.0),
                ("b".into(), "r".into(), 20.0)
            ]
        );
    }

    /// Runner is part of the series, so saving on one class leaves the
    /// other's numbers alone.
    #[test]
    fn a_save_on_another_runner_keeps_both() {
        let (_tmp, s) = store();
        s.save("x", &[rec("a", "r1", 10.0)]).unwrap();
        s.save("x", &[rec("a", "r2", 99.0)]).unwrap();
        assert_eq!(s.load("x").unwrap().records.len(), 2);
    }

    /// An older tak must not delete what a newer one saved.
    #[test]
    fn lines_it_cannot_read_survive_a_save() {
        let (_tmp, s) = store();
        std::fs::create_dir_all(s.dir()).unwrap();
        let mut future = rec("a", "r", 1.0);
        future.v = SCHEMA_VERSION + 1;
        let future = future.to_line().unwrap();
        std::fs::write(s.path("x"), format!("{future}\nnot json\n")).unwrap();
        s.save("x", &[rec("a", "r", 2.0)]).unwrap();
        let body = std::fs::read_to_string(s.path("x")).unwrap();
        assert!(body.contains(&future), "{body}");
        assert!(body.contains("not json"), "{body}");
        assert_eq!(s.load("x").unwrap().records.len(), 1);
    }

    #[test]
    fn a_missing_baseline_names_the_saved_ones() {
        let (_tmp, s) = store();
        let e = s.load("before").unwrap_err().to_string();
        assert!(e.contains("none saved yet"), "{e}");
        s.save("main", &[rec("a", "r", 1.0)]).unwrap();
        s.save("wip", &[rec("a", "r", 1.0)]).unwrap();
        let e = s.load("before").unwrap_err().to_string();
        assert!(e.contains("saved: main, wip"), "{e}");
    }

    #[test]
    fn a_baseline_with_nothing_readable_is_an_error() {
        let (_tmp, s) = store();
        std::fs::create_dir_all(s.dir()).unwrap();
        std::fs::write(s.path("x"), "not json\n").unwrap();
        assert!(s.load("x").is_err());
    }

    /// Two saves of the same records in a different order write the same
    /// bytes, so the file never changes for a reason that is not the data.
    #[test]
    fn the_file_does_not_depend_on_save_order() {
        let (_tmp, s1) = store();
        let (_tmp2, s2) = store();
        s1.save("x", &[rec("a", "r", 1.0), rec("b", "r", 2.0)])
            .unwrap();
        s2.save("x", &[rec("b", "r", 2.0), rec("a", "r", 1.0)])
            .unwrap();
        assert_eq!(
            std::fs::read(s1.path("x")).unwrap(),
            std::fs::read(s2.path("x")).unwrap()
        );
    }

    #[test]
    fn only_the_benchmarks_this_run_measured_are_compared() {
        let baseline = [
            rec("a", "r1", 1.0),
            rec("a", "r2", 1.0),
            rec("b", "r1", 1.0),
        ];
        let kept = relevant(&baseline, &[rec("a", "r1", 2.0)]);
        // `b` was not run; `a` on the other runner stays, so the report can
        // say the runners differ.
        let runners: Vec<_> = kept
            .iter()
            .map(|r| (r.bench.as_str(), r.runner.as_str()))
            .collect();
        assert_eq!(runners, vec![("a", "r1"), ("a", "r2")]);
    }
}
