//! Compare one commit's measurements against another's.
//!
//! This is the part that can block a pull request, so it is deliberately narrow
//! about what it will fail on. Only instruction counts gate. Wall clock is
//! reported beside them and never gated: on identical hardware it moves 4-20%
//! run to run, so a threshold tight enough to catch a real regression fires
//! constantly, and one loose enough to stay quiet catches nothing.
//!
//! Rendering lives in [`markdown`], separate from the comparison itself, so a
//! chart or a different report format is a new function rather than a rewrite.

use crate::record::Record;
use std::collections::{BTreeMap, BTreeSet};

/// The only metric a gate may fire on.
pub const GATED_METRIC: &str = "instructions";

/// The timing metric shown alongside it, for context only.
const WALL_METRIC: &str = "wall_min_ms";

/// What identifies a comparable series.
///
/// Runner is part of the key because it has to be: absolute counts shift
/// between machine types by more than a real regression does, so comparing a
/// measurement taken on one runner against another's is not a comparison at
/// all. Two commits measured on different runners simply do not line up here,
/// which is the correct outcome rather than a missing feature.
pub type Key = (String, String, String);

/// One metric, on one series, on both sides.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub bench: String,
    pub tool: String,
    pub runner: String,
    pub metric: String,
    pub base: f64,
    pub head: f64,
}

impl Change {
    /// Change as a percentage of the base. Zero when the base is zero, since
    /// the alternative is an infinity that renders as noise.
    pub fn pct(&self) -> f64 {
        if self.base == 0.0 {
            return 0.0;
        }
        (self.head - self.base) / self.base * 100.0
    }

    /// Is this a regression the gate should fail on?
    pub fn regressed(&self, gate_pct: f64) -> bool {
        self.metric == GATED_METRIC && self.pct() > gate_pct
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Comparison {
    /// Every metric present on both sides, in a stable order.
    pub changes: Vec<Change>,
    /// Series measured on the head commit and not the base — a new benchmark,
    /// or the first run on a new runner class. Reported, never gated: there is
    /// nothing to compare against.
    pub added: Vec<Key>,
    /// Series on the base and not the head. Usually a benchmark that was
    /// removed, occasionally a run that failed to record — worth surfacing
    /// either way, because a silently vanishing benchmark stops gating.
    pub removed: Vec<Key>,
}

impl Comparison {
    pub fn regressions(&self, gate_pct: f64) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|c| c.regressed(gate_pct))
            .collect()
    }

    /// True when there is nothing to compare — no overlapping series at all.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Fold records into one value per (series, metric), taking the minimum.
///
/// A commit can carry several records for the same series: CI re-run, a retry,
/// a local run pushed alongside. The minimum is the right reducer for the same
/// reason tak reports the minimum within a run — the extra work a machine
/// sometimes does is one-sided. Averaging would let one noisy sample move a
/// number that is supposed to be deterministic.
fn index(records: &[Record]) -> BTreeMap<(Key, String), f64> {
    let mut out: BTreeMap<(Key, String), f64> = BTreeMap::new();
    for r in records {
        let key: Key = (r.bench.clone(), r.tool.clone(), r.runner.clone());
        for (metric, value) in &r.metrics {
            out.entry((key.clone(), metric.clone()))
                .and_modify(|existing| {
                    if value < existing {
                        *existing = *value;
                    }
                })
                .or_insert(*value);
        }
    }
    out
}

/// Compare two sets of records.
pub fn compare(base: &[Record], head: &[Record]) -> Comparison {
    let (b, h) = (index(base), index(head));

    let mut changes = Vec::new();
    for ((key, metric), head_value) in &h {
        if let Some(base_value) = b.get(&(key.clone(), metric.clone())) {
            changes.push(Change {
                bench: key.0.clone(),
                tool: key.1.clone(),
                runner: key.2.clone(),
                metric: metric.clone(),
                base: *base_value,
                head: *head_value,
            });
        }
    }

    let base_keys: BTreeSet<Key> = b.keys().map(|(k, _)| k.clone()).collect();
    let head_keys: BTreeSet<Key> = h.keys().map(|(k, _)| k.clone()).collect();

    Comparison {
        changes,
        added: head_keys.difference(&base_keys).cloned().collect(),
        removed: base_keys.difference(&head_keys).cloned().collect(),
    }
}

/// `12345678` -> `12,345,678`
fn thousands(v: f64) -> String {
    let n = format!("{:.0}", v.abs());
    let mut out = String::new();
    for (i, c) in n.chars().enumerate() {
        if i > 0 && (n.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if v < 0.0 { format!("-{out}") } else { out }
}

fn signed_pct(p: f64) -> String {
    format!("{}{:.2}%", if p >= 0.0 { "+" } else { "" }, p)
}

/// Render a comparison as markdown, suitable for a PR comment or a terminal.
///
/// One format rather than two. A markdown table is readable unrendered, so a
/// second plain-text renderer would be a second thing to keep correct for no
/// gain.
pub fn markdown(c: &Comparison, gate_pct: f64) -> String {
    let mut out = String::new();

    if c.is_empty() {
        out.push_str(
            "No overlapping measurements to compare. Either the base commit has \
             none recorded, or the two were measured on different runner classes \
             — counts are not comparable across machine types.\n",
        );
        return out;
    }

    // One row per series, both metrics side by side: reading them together is
    // what tells you whether a wall-clock move is real.
    let mut series: BTreeMap<Key, (Option<&Change>, Option<&Change>)> = BTreeMap::new();
    for change in &c.changes {
        let key = (
            change.bench.clone(),
            change.tool.clone(),
            change.runner.clone(),
        );
        let slot = series.entry(key).or_insert((None, None));
        match change.metric.as_str() {
            GATED_METRIC => slot.0 = Some(change),
            WALL_METRIC => slot.1 = Some(change),
            _ => {}
        }
    }

    out.push_str("| benchmark | instructions | Δ | wall (min) | Δ |\n");
    out.push_str("|---|---:|---:|---:|---:|\n");
    for ((bench, tool, _runner), (ins, wall)) in &series {
        let name = if tool == "self" {
            bench.clone()
        } else {
            format!("{bench} ({tool})")
        };
        let (ins_cell, ins_delta) = match ins {
            Some(ch) => {
                let flag = if ch.regressed(gate_pct) {
                    " ⚠️"
                } else {
                    ""
                };
                (
                    format!("{} → {}", thousands(ch.base), thousands(ch.head)),
                    format!("**{}**{flag}", signed_pct(ch.pct())),
                )
            }
            None => ("—".into(), "—".into()),
        };
        let (wall_cell, wall_delta) = match wall {
            Some(ch) => (
                format!("{:.2} → {:.2}ms", ch.base, ch.head),
                signed_pct(ch.pct()),
            ),
            None => ("—".into(), "—".into()),
        };
        out.push_str(&format!(
            "| {name} | {ins_cell} | {ins_delta} | {wall_cell} | {wall_delta} |\n"
        ));
    }

    let regressions = c.regressions(gate_pct);
    out.push('\n');
    if regressions.is_empty() {
        out.push_str(&format!(
            "No instruction-count regression above {gate_pct}%.\n"
        ));
    } else {
        out.push_str(&format!(
            "**{} benchmark(s) above the {gate_pct}% gate:** {}\n",
            regressions.len(),
            regressions
                .iter()
                .map(|c| format!("`{}` {}", c.bench, signed_pct(c.pct())))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if !c.added.is_empty() {
        out.push_str(&format!(
            "\nNew, nothing to compare against: {}\n",
            c.added
                .iter()
                .map(|(b, _, _)| format!("`{b}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !c.removed.is_empty() {
        out.push_str(&format!(
            "\nMeasured on the base but not here — a benchmark that stops \
             running also stops gating: {}\n",
            c.removed
                .iter()
                .map(|(b, _, _)| format!("`{b}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Said once, in the report, rather than left for someone to rediscover.
    out.push_str(
        "\n<sub>Only instruction counts gate. Wall clock is shown for context — \
         on identical hardware it moves 4-20% run to run.</sub>\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(bench: &str, runner: &str, ins: f64, wall: f64) -> Record {
        Record {
            v: 1,
            bench: bench.into(),
            tool: "self".into(),
            version: None,
            runner: runner.into(),
            ts: "2026-01-01T00:00:00Z".into(),
            metrics: BTreeMap::from([
                (GATED_METRIC.to_string(), ins),
                (WALL_METRIC.to_string(), wall),
            ]),
        }
    }

    #[test]
    fn a_rise_beyond_the_gate_is_a_regression() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 1_020_000.0, 10.0)],
        );
        assert_eq!(c.regressions(1.0).len(), 1, "2% should trip a 1% gate");
        assert!(c.regressions(5.0).is_empty(), "2% should not trip 5%");
    }

    #[test]
    fn an_improvement_never_gates() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 500_000.0, 10.0)],
        );
        assert!(c.regressions(1.0).is_empty());
        assert_eq!(
            c.changes
                .iter()
                .find(|c| c.metric == GATED_METRIC)
                .unwrap()
                .pct()
                .round(),
            -50.0
        );
    }

    /// The reason the gate is narrow. A doubling of wall clock is ordinary
    /// noise on a shared runner and must not fail anyone's pull request.
    #[test]
    fn wall_clock_never_gates_however_bad_it_looks() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 1_000_000.0, 100.0)],
        );
        assert!(c.regressions(0.001).is_empty());
        let wall = c.changes.iter().find(|c| c.metric == WALL_METRIC).unwrap();
        assert_eq!(wall.pct().round(), 900.0);
    }

    /// Different runner classes are different series. Comparing across them
    /// would report a machine change as a code change.
    #[test]
    fn a_different_runner_is_not_a_comparison() {
        let c = compare(
            &[rec("a", "gha-linux", 1_000_000.0, 10.0)],
            &[rec("a", "gha-macos", 2_000_000.0, 10.0)],
        );
        assert!(c.is_empty(), "nothing should line up");
        assert_eq!(c.added.len(), 1);
        assert_eq!(c.removed.len(), 1);
        assert!(c.regressions(1.0).is_empty());
    }

    #[test]
    fn a_new_benchmark_is_reported_but_not_gated() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("b", "gha", 9.9e9, 10.0),
            ],
        );
        assert_eq!(
            c.added,
            vec![("b".to_string(), "self".to_string(), "gha".to_string())]
        );
        assert!(c.regressions(1.0).is_empty());
    }

    /// A benchmark that stops running stops gating, so its absence has to be
    /// visible rather than simply making the table shorter.
    #[test]
    fn a_vanished_benchmark_is_surfaced() {
        let c = compare(
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("b", "gha", 1.0, 1.0),
            ],
            &[rec("a", "gha", 1_000_000.0, 10.0)],
        );
        assert_eq!(c.removed.len(), 1);
        assert!(markdown(&c, 1.0).contains("stops gating"));
    }

    /// Several records for one series collapse to the minimum, not the mean:
    /// a noisy re-run must not move a number that is meant to be deterministic.
    #[test]
    fn duplicate_records_reduce_to_the_minimum() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[
                rec("a", "gha", 3_000_000.0, 30.0),
                rec("a", "gha", 1_000_000.0, 10.0),
            ],
        );
        assert!(c.regressions(1.0).is_empty(), "the clean sample should win");
    }

    #[test]
    fn nothing_in_common_says_so_rather_than_passing_quietly() {
        let c = compare(&[], &[rec("a", "gha", 1.0, 1.0)]);
        assert!(c.is_empty());
        assert!(markdown(&c, 1.0).contains("No overlapping measurements"));
    }

    #[test]
    fn thousands_separates() {
        assert_eq!(thousands(1234567.0), "1,234,567");
        assert_eq!(thousands(999.0), "999");
        assert_eq!(thousands(1000.0), "1,000");
    }
}
