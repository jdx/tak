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
use crate::record::{Record, SCHEMA_VERSION, parse_note};

/// The schema version of a line written by a newer tak, if it is one.
///
/// Read from the `v` field alone, the one field every schema version keeps,
/// so it works whatever else a newer record changed.
fn newer_schema(line: &str) -> Option<u64> {
    let v = serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("v")?
        .as_u64()?;
    (v > u64::from(SCHEMA_VERSION)).then_some(v)
}

/// Refuse to save over a baseline that holds a record from a newer schema.
///
/// Refused rather than kept or dropped. Kept, the newer line sits beside this
/// run's line for the same series, and once tak is upgraded again the reader
/// folds both to their minimum, so a stale value can win. Dropped, an older
/// tak destroys what a newer one saved. And this version cannot tell which
/// series the line belongs to: a newer schema may spell its key differently.
/// A downgrade is rare, and another name costs nothing.
fn refuse_newer(name: &str, path: &Path, body: &str) -> Result<()> {
    if let Some(v) = body.lines().find_map(newer_schema) {
        bail!(
            "baseline `{name}` holds records from a newer tak (schema {v}; this tak writes \
             {SCHEMA_VERSION}), and saving over it could leave a stale value beside this \
             run's. Save under another name, or upgrade tak ({})",
            path.display()
        );
    }
    Ok(())
}

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

    /// Fail now if saving under `name` would be refused later, so a run
    /// that cannot be saved is not measured first.
    pub fn check_saveable(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let path = self.path(name);
        match std::fs::read_to_string(&path) {
            Ok(body) => refuse_newer(name, &path, &body),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }

    /// Save `records` under `name`, returning the file written.
    ///
    /// Replaces what the baseline held for each series in `records` and keeps
    /// every other series, so `tak run --bench a --save-baseline x` updates `a`
    /// without discarding the `b` a full run saved earlier. The same series key
    /// `compare` uses, runner included: a baseline saved with `--runner` on two
    /// classes keeps one series for each.
    ///
    /// A line that does not parse at all — a hand edit — is kept as it is:
    /// readers skip it, and dropping it would destroy something someone wrote.
    /// A record from a newer schema refuses the save instead; see the comment
    /// where it is found.
    ///
    /// Written to a temporary file and renamed into place, so an interrupted
    /// save leaves the previous baseline whole rather than half of either.
    ///
    /// The read-modify-write holds an exclusive lock on a sibling lock file.
    /// Every worktree of a clone shares this directory, so two runs saving
    /// different benchmarks to one name at once is ordinary. Without the
    /// lock both would read the old file, and whichever renamed last would
    /// silently drop the other's series while both reported success. An OS
    /// lock rather than an exclusively created marker file, because the OS
    /// releases it when a process dies; a marker left by a killed run would
    /// block every save after it until someone deleted it by hand.
    pub fn save(&self, name: &str, records: &[Record]) -> Result<PathBuf> {
        validate_name(name)?;
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("could not create {}", self.dir.display()))?;
        // Hidden and without the baseline extension, so `list` never shows it.
        let lock_path = self.dir.join(format!(".{name}.lock"));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("could not open {}", lock_path.display()))?;
        lock.lock()
            .with_context(|| format!("could not lock {}", lock_path.display()))?;

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
        // Checked again under the lock: `check_saveable` ran before measuring,
        // and a newer tak in another worktree may have saved since.
        refuse_newer(name, &path, &existing)?;
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
        // Only now: the next writer must read the file this one renamed in.
        drop(lock);
        Ok(path)
    }
}

/// Why a series this run measured cannot be gated against the baseline.
#[derive(Debug, Clone, PartialEq)]
pub enum Gap {
    /// This run asked for a count, valgrind was present, and counting
    /// failed. Whatever the baseline holds, nothing here can be compared.
    CountFailed,
    /// The baseline counted it on this runner class and this run did not:
    /// valgrind missing, counting failed, or counters turned off.
    NotCountedHere,
    /// This run counted it and the baseline holds it on this runner class
    /// without a count.
    SavedWithoutCount,
    /// This run counted it and the baseline holds it only on these other
    /// runner classes, which are never compared with this one.
    OnlyOnOtherRunners(Vec<String>),
    /// This run counted it and the baseline has never seen it: new since the
    /// baseline was saved.
    NotInBaseline,
    /// This run did not count it, and the baseline counted it only on these
    /// other runner classes.
    UncountedAndOnlyOnOtherRunners(Vec<String>),
}

fn classes(runners: &[String]) -> String {
    runners
        .iter()
        .map(|r| format!("`{r}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl std::fmt::Display for Gap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Gap::CountFailed => write!(f, "instruction counting failed in this run"),
            Gap::NotCountedHere => write!(f, "not counted in this run"),
            Gap::SavedWithoutCount => write!(f, "saved without an instruction count"),
            Gap::OnlyOnOtherRunners(runners) => {
                write!(f, "saved only on runner class {}", classes(runners))
            }
            Gap::NotInBaseline => write!(f, "not in the baseline"),
            Gap::UncountedAndOnlyOnOtherRunners(runners) => write!(
                f,
                "not counted in this run, and counted in the baseline only on runner class {}",
                classes(runners)
            ),
        }
    }
}

/// The series this run measured that `--gate` cannot check, and why.
///
/// A benchmark whose count this run failed to take, one added since the
/// baseline was saved, or one the baseline holds only for another runner
/// class would otherwise pass a gate that checked only its neighbours.
///
/// Judged per series of *this run*, so on this run's runner class only. A
/// baseline saved on two classes holds a series for each, and the one for the
/// other class is not missing from this run — it was never going to be
/// compared with it. Counting it as a gap failed every gate on both classes.
///
/// A series with no count on either side is left out: a subject with
/// `counters = false` is wall-clock only by design and never gates. That rule
/// cannot tell design from accident by the records alone, which look the same
/// either way, so `count_failed` names the series whose count this run asked
/// for, with valgrind present, and did not get. Those are always a gap: a new
/// benchmark whose count failed has nothing in the baseline either, and was
/// otherwise skipped while `--gate` passed on its neighbours.
pub fn gaps(
    baseline: &[Record],
    current: &[Record],
    count_failed: &BTreeSet<Key>,
) -> Vec<(Key, Gap)> {
    let counted = |r: &Record| r.metrics.contains_key(crate::compare::GATED_METRIC);
    let mut out = Vec::new();
    let keys: BTreeSet<Key> = current.iter().map(key).collect();
    for k in keys {
        let here = current.iter().filter(|r| key(r) == k).any(counted);
        if !here && count_failed.contains(&k) {
            out.push((k, Gap::CountFailed));
            continue;
        }
        let same: Vec<&Record> = baseline.iter().filter(|r| key(r) == k).collect();
        let saved = same.iter().any(|r| counted(r));
        let gap = match (here, saved) {
            (true, true) => continue,
            // Uncounted on both sides of this class: wall-clock only by
            // design, unless this class is absent and another class counted
            // it. Then the benchmark is one the project gates, and this run
            // neither counted it nor has anything to hold it to. Skipping it
            // let `--gate` pass on the strength of the other benchmarks.
            (false, false) => {
                if !same.is_empty() {
                    continue;
                }
                let elsewhere = counted_runners(baseline, &k);
                if elsewhere.is_empty() {
                    continue;
                }
                Gap::UncountedAndOnlyOnOtherRunners(elsewhere)
            }
            (false, true) => Gap::NotCountedHere,
            (true, false) if !same.is_empty() => Gap::SavedWithoutCount,
            (true, false) => {
                let elsewhere = other_runners(baseline, &k);
                if elsewhere.is_empty() {
                    Gap::NotInBaseline
                } else {
                    Gap::OnlyOnOtherRunners(elsewhere)
                }
            }
        };
        out.push((k, gap));
    }
    out
}

/// Runner classes other than `k`'s that the baseline holds `k`'s benchmark
/// and tool on, sorted.
pub fn other_runners(baseline: &[Record], k: &Key) -> Vec<String> {
    let runners: BTreeSet<&str> = baseline
        .iter()
        .filter(|r| r.bench == k.0 && r.tool == k.1 && r.runner != k.2)
        .map(|r| r.runner.as_str())
        .collect();
    runners.into_iter().map(str::to_string).collect()
}

/// Like [`other_runners`], but only the classes that saved an instruction
/// count for it.
fn counted_runners(baseline: &[Record], k: &Key) -> Vec<String> {
    let runners: BTreeSet<&str> = baseline
        .iter()
        .filter(|r| r.bench == k.0 && r.tool == k.1 && r.runner != k.2)
        .filter(|r| r.metrics.contains_key(crate::compare::GATED_METRIC))
        .map(|r| r.runner.as_str())
        .collect();
    runners.into_iter().map(str::to_string).collect()
}

/// The part of a baseline this run is compared against: the series this run
/// measured, on this run's runner class.
///
/// A baseline outlives the run that saved it, so `tak run --bench a` against
/// one saved by a full run would otherwise list every other benchmark as
/// "measured on the base but not here". That warning exists for a CI job that
/// silently stopped running something; here it would only be noise. The same
/// goes for the series a baseline holds for other runner classes: they are
/// not comparable, and not missing either. When one of them is all the
/// baseline has for a benchmark, [`other_runners`] names it in a warning and
/// [`gaps`] keeps `--gate` from passing over it.
pub fn relevant(baseline: &[Record], current: &[Record]) -> Vec<Record> {
    let measured: BTreeSet<Key> = current.iter().map(key).collect();
    baseline
        .iter()
        .filter(|r| measured.contains(&key(r)))
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

    /// Heap-allocation metrics survive a save and load, and a `--baseline`
    /// report renders them through the same allocation table as `tak
    /// compare`, still without gating on them.
    #[test]
    fn allocation_metrics_reach_a_baseline_report() {
        let with_allocs = |blocks: f64| {
            let mut r = rec("a", "r", 10.0);
            r.metrics.insert("alloc_blocks".into(), blocks);
            r.metrics.insert("alloc_bytes".into(), blocks * 8.0);
            r.metrics.insert("alloc_peak_bytes".into(), blocks * 4.0);
            r
        };
        let (_tmp, s) = store();
        s.save("before", &[with_allocs(10.0)]).unwrap();
        let b = s.load("before").unwrap();
        let current = [with_allocs(20.0)];
        let c = crate::compare::compare(&relevant(&b.records, &current), &current);
        let gates = crate::compare::Gates::uniform(crate::compare::Gate::new(1.0, 0).unwrap());
        let md = crate::compare::markdown(&c, &crate::compare::Trend::new(), &gates, false);
        assert!(md.contains("Heap allocations"), "{md}");
        assert!(md.contains("| a | 10 → 20 | +100.00% |"), "{md}");
        assert!(c.regressions(&gates).is_empty());
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

    /// A line that is not a record at all — a hand edit — survives a save.
    #[test]
    fn a_malformed_line_survives_a_save() {
        let (_tmp, s) = store();
        std::fs::create_dir_all(s.dir()).unwrap();
        std::fs::write(s.path("x"), "not json\n").unwrap();
        s.save("x", &[rec("a", "r", 2.0)]).unwrap();
        let body = std::fs::read_to_string(s.path("x")).unwrap();
        assert!(body.contains("not json"), "{body}");
        assert_eq!(s.load("x").unwrap().records.len(), 1);
    }

    /// An older tak neither deletes a newer one's record nor writes beside
    /// it: kept next to this run's line for the same series, the stale newer
    /// value would win the minimum once tak is upgraded again. So the save is
    /// refused, and the file is left exactly as it was.
    #[test]
    fn a_newer_schema_record_refuses_the_save() {
        let (_tmp, s) = store();
        std::fs::create_dir_all(s.dir()).unwrap();
        let mut future = rec("a", "r", 1.0);
        future.v = SCHEMA_VERSION + 1;
        let body = format!("{}\n", future.to_line().unwrap());
        std::fs::write(s.path("x"), &body).unwrap();
        let e = s.save("x", &[rec("a", "r", 2.0)]).unwrap_err().to_string();
        assert!(e.contains("holds records from a newer tak"), "{e}");
        assert!(e.contains("Save under another name"), "{e}");
        assert_eq!(std::fs::read_to_string(s.path("x")).unwrap(), body);
        // Another name is unaffected.
        s.save("y", &[rec("a", "r", 2.0)]).unwrap();
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

    /// Concurrent saves of different benchmarks to one name, as two
    /// worktrees would do, must all survive. Threads rather than processes:
    /// the lock is on an open file description, so two opens in one process
    /// contend exactly as two processes would.
    #[test]
    fn concurrent_saves_keep_every_series() {
        let (_tmp, s) = store();
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let s = s.clone();
                std::thread::spawn(move || {
                    s.save("x", &[rec(&format!("b{i}"), "r", i as f64)])
                        .unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(s.load("x").unwrap().records.len(), 16);
        assert_eq!(
            s.list().unwrap(),
            vec!["x".to_string()],
            "the lock file is listed"
        );
    }

    /// The gaps of a run in which no count failed.
    fn gaps(base: &[Record], head: &[Record]) -> Vec<(Key, Gap)> {
        super::gaps(base, head, &BTreeSet::new())
    }

    /// A count that was asked for, with valgrind present, and failed is a
    /// gap whatever the baseline holds — including nothing at all, for a
    /// benchmark added since it was saved. Counters off by design is not.
    #[test]
    fn a_failed_count_is_a_gap_and_counters_off_is_not() {
        let k: Key = ("new".into(), "self".into(), "r".into());
        let head = [wall_only("new", "r")];
        assert_eq!(
            super::gaps(&[], &head, &BTreeSet::from([k.clone()])),
            vec![(k.clone(), Gap::CountFailed)]
        );
        assert_eq!(
            super::gaps(
                &[wall_only("new", "r")],
                &head,
                &BTreeSet::from([k.clone()])
            ),
            vec![(k, Gap::CountFailed)]
        );
        assert_eq!(gaps(&[], &head), vec![]);
    }

    fn wall_only(bench: &str, runner: &str) -> Record {
        let mut r = rec(bench, runner, 0.0);
        r.metrics = BTreeMap::from([("wall_min_ms".to_string(), 1.0)]);
        r
    }

    #[test]
    fn a_series_this_run_cannot_gate_is_a_gap() {
        let base = [
            rec("a", "r", 1.0),
            rec("b", "r", 1.0),
            wall_only("s", "r"),
            wall_only("w", "r"),
        ];
        // `b` lost its count this run; `c` is new; `s` was saved uncounted;
        // `w` never had a count on either side and is wall-clock only.
        let head = [
            rec("a", "r", 1.0),
            wall_only("b", "r"),
            rec("c", "r", 1.0),
            rec("s", "r", 1.0),
            wall_only("w", "r"),
        ];
        let got: Vec<(String, Gap)> = gaps(&base, &head)
            .into_iter()
            .map(|(k, g)| (k.0, g))
            .collect();
        assert_eq!(
            got,
            vec![
                ("b".into(), Gap::NotCountedHere),
                ("c".into(), Gap::NotInBaseline),
                ("s".into(), Gap::SavedWithoutCount),
            ]
        );
    }

    /// A baseline saved on two runner classes holds a series for each. The
    /// other class's is not this run's, so it is not a gap: counting it as
    /// one failed every gate on both classes.
    #[test]
    fn another_runner_is_ignored_when_this_runner_is_saved() {
        let base = [rec("a", "r1", 1.0), rec("a", "r2", 1.0)];
        assert_eq!(gaps(&base, &[rec("a", "r1", 1.0)]), vec![]);
        assert_eq!(gaps(&base, &[rec("a", "r2", 1.0)]), vec![]);
    }

    /// When no saved class matches, the gap names the classes there are, so
    /// the message cannot suggest saving on one that is already present.
    #[test]
    fn no_matching_runner_is_a_gap_naming_the_saved_ones() {
        let base = [rec("a", "r1", 1.0), rec("a", "r2", 1.0)];
        let got = gaps(&base, &[rec("a", "r3", 1.0)]);
        assert_eq!(
            got[0].1,
            Gap::OnlyOnOtherRunners(vec!["r1".into(), "r2".into()])
        );
        assert_eq!(
            got[0].1.to_string(),
            "saved only on runner class `r1`, `r2`"
        );
    }

    /// Uncounted here and counted only on another class: a gated benchmark
    /// this run neither counted nor can compare, which a gate must not skip.
    /// Uncounted on this class on both sides stays wall-clock only.
    #[test]
    fn uncounted_here_and_counted_only_elsewhere_is_a_gap() {
        let base = [rec("a", "r1", 1.0)];
        let got = gaps(&base, &[wall_only("a", "r2")]);
        assert_eq!(
            got[0].1,
            Gap::UncountedAndOnlyOnOtherRunners(vec!["r1".into()])
        );
        assert_eq!(
            gaps(&[wall_only("a", "r1")], &[wall_only("a", "r2")]),
            vec![]
        );
        assert_eq!(
            gaps(
                &[rec("a", "r1", 1.0), wall_only("a", "r2")],
                &[wall_only("a", "r2")]
            ),
            vec![]
        );
    }

    #[test]
    fn only_this_runs_series_are_compared() {
        let baseline = [
            rec("a", "r1", 1.0),
            rec("a", "r2", 1.0),
            rec("b", "r1", 1.0),
        ];
        let kept = relevant(&baseline, &[rec("a", "r1", 2.0)]);
        // `b` was not run, and `a` on `r2` is another class's series.
        let runners: Vec<_> = kept
            .iter()
            .map(|r| (r.bench.as_str(), r.runner.as_str()))
            .collect();
        assert_eq!(runners, vec![("a", "r1")]);
    }
}
