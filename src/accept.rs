//! Declared, intentional regressions: which benchmarks a change says it made
//! more expensive on purpose, and who said so.
//!
//! The only other way past a failing gate is `--no-gate`, which stops gating
//! every benchmark at once. A team with one deliberate cost increase then either
//! loosens the threshold for everything or stops requiring the check — both of
//! which leave the next, unintended regression unguarded. An acceptance names
//! one benchmark, and it is always reported with where it came from, so the
//! override is as visible in the report as the regression it covers.
//!
//! Two sources, deliberately no more:
//!
//! - `tak compare --accept`, for a CI integration that maps something outside
//!   the commits — a pull-request label only maintainers can apply, say — onto
//!   an acceptance. This is the one honoured by default, because whoever
//!   decides it is not necessarily whoever wrote the change; and
//! - a `Tak-Accept:` trailer on a commit in the compared range, which is how a
//!   change carries its own justification into history. Opt-in through the
//!   `accept_trailers` setting: those commits are the change being gated, and
//!   by default a change must not be able to waive its own gate.
//!
//! Names only, no bound such as `startup<=5%`. A bound invites arguing about
//! the number in the trailer rather than looking at the change, and a second
//! syntax is a second thing to get wrong in a line nobody tests. An accepted
//! benchmark still shows its full change in the report.

use std::collections::{BTreeMap, BTreeSet};

/// The trailer key. git matches trailer keys case-insensitively, so
/// `tak-accept:` works too; this is the spelling the docs and report use.
pub const TRAILER: &str = "Tak-Accept";

/// Where an acceptance came from. Always shown beside it: an override nobody
/// can trace is exactly the kind of quiet gate-weakening this exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// `tak compare --accept`.
    Flag,
    /// A trailer on this commit, by full SHA.
    Trailer(String),
}

impl Source {
    fn describe(&self) -> String {
        match self {
            Source::Flag => "`--accept`".to_string(),
            // Twelve characters, matching how `tak history` names commits.
            Source::Trailer(sha) => {
                format!("`{TRAILER}` in `{}`", &sha[..sha.len().min(12)])
            }
        }
    }
}

/// Accepted benchmark names, each with every source that accepted it.
///
/// Keyed on the benchmark name alone, so an acceptance covers every tool and
/// runner that benchmark was measured with. The name is what a user declares
/// in `tak.toml` and sees in the report; the runner in particular is something
/// a commit author cannot know ahead of time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Acceptances(BTreeMap<String, BTreeSet<Source>>);

impl Acceptances {
    /// Add every name in a comma-separated list, as a trailer value is read.
    ///
    /// Commas, because a trailer is one line and `Tak-Accept: a, b` is how
    /// people write two names there; repeated trailers work as well. Nothing
    /// else is stripped: `Tak-Accept: startup (new plugin loader)` names a
    /// benchmark that does not exist and is reported as such, which fails
    /// closed — the real `startup` regression still gates — rather than
    /// guessing which word was meant.
    pub fn add(&mut self, list: &str, source: Source) {
        for name in list.split(',') {
            self.add_name(name, source.clone());
        }
    }

    /// Add one exact name, as `--accept` gives it.
    ///
    /// Not split on commas. Benchmark names are unrestricted, so a name that
    /// contains a comma would otherwise have no spelling that accepts it; the
    /// flag is repeatable, which covers every list without needing a separator.
    pub fn add_name(&mut self, name: &str, source: Source) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.0.entry(name.to_string()).or_default().insert(source);
    }

    /// Parse the output of [`crate::notes::trailers`]: one line per commit,
    /// the SHA, a NUL, and that commit's trailer values joined by commas.
    pub fn add_trailer_log(&mut self, log: &str) {
        for line in log.lines() {
            let Some((sha, values)) = line.split_once('\0') else {
                continue;
            };
            self.add(values, Source::Trailer(sha.to_string()));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn covers(&self, bench: &str) -> bool {
        self.0.contains_key(bench)
    }

    /// Every accepted name with its sources, in a stable order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &BTreeSet<Source>)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Where `bench` was accepted from, for the report.
    pub fn describe(&self, bench: &str) -> String {
        self.0
            .get(bench)
            .map(|sources| {
                sources
                    .iter()
                    .map(Source::describe)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn a_list_splits_on_commas_and_trims() {
        let mut a = Acceptances::default();
        a.add(" startup, resolve ,,", Source::Flag);
        let names: Vec<_> = a.iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["resolve", "startup"]);
    }

    /// A commit with no trailer still prints its SHA; it must add nothing
    /// rather than an empty name that could never match and would be reported.
    #[test]
    fn a_commit_without_the_trailer_accepts_nothing() {
        let mut a = Acceptances::default();
        a.add_trailer_log(&format!("{A}\0\n{B}\0startup\n"));
        assert!(a.covers("startup"));
        assert_eq!(a.iter().count(), 1);
        assert_eq!(a.describe("startup"), "`Tak-Accept` in `bbbbbbbbbbbb`");
    }

    /// Every source is kept, not just the first: the report has to name all
    /// of them, or removing one trailer could silently leave another in force.
    #[test]
    fn every_source_of_an_acceptance_is_kept() {
        let mut a = Acceptances::default();
        a.add_trailer_log(&format!("{A}\0startup,resolve\n{B}\0startup\n"));
        a.add("startup", Source::Flag);
        assert_eq!(
            a.describe("startup"),
            "`--accept`, `Tak-Accept` in `aaaaaaaaaaaa`, `Tak-Accept` in `bbbbbbbbbbbb`"
        );
        assert_eq!(a.describe("resolve"), "`Tak-Accept` in `aaaaaaaaaaaa`");
    }

    /// The flag takes a name as given, so a benchmark whose name contains a
    /// comma can still be accepted.
    #[test]
    fn a_flag_value_is_one_name() {
        let mut a = Acceptances::default();
        a.add_name("parse a,b", Source::Flag);
        assert!(a.covers("parse a,b"));
        assert_eq!(a.iter().count(), 1);
    }

    /// A reason written after the name is not quietly dropped. The whole value
    /// is the name, which matches nothing, so the regression still gates and
    /// the report says the acceptance named no benchmark.
    #[test]
    fn a_reason_in_the_value_is_not_guessed_at() {
        let mut a = Acceptances::default();
        a.add("startup (new loader)", Source::Flag);
        assert!(!a.covers("startup"));
        assert!(a.covers("startup (new loader)"));
    }
}
