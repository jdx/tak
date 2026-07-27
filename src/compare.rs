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
    /// Change as a percentage of the base, or `None` when the base is zero.
    ///
    /// `None` rather than `0.0`. Returning zero was worse than imprecise: the
    /// gate compares this against the threshold, so a rise from a zero baseline
    /// read as no change at all and passed silently, however large the head.
    /// A percentage of nothing is undefined, and the type should say so.
    pub fn pct(&self) -> Option<f64> {
        if self.base == 0.0 {
            return None;
        }
        Some((self.head - self.base) / self.base * 100.0)
    }

    /// Is this a regression the gate should fail on?
    ///
    /// From a zero base, any increase counts. There is no threshold that means
    /// anything against zero, and passing would be the one outcome that is
    /// certainly wrong.
    pub fn regressed(&self, gate_pct: f64) -> bool {
        if self.metric != GATED_METRIC {
            return false;
        }
        match self.pct() {
            Some(p) => p > gate_pct,
            None => self.head > 0.0,
        }
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

/// Recent values for each series, oldest first.
pub type Trend = BTreeMap<Key, Vec<f64>>;

/// Eight levels of block, low to high.
const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A sparkline, scaled to the series' own range.
///
/// Per-series scaling is the only useful choice: an install benchmark at 100M
/// instructions and a startup one at 7M share no axis, and a shared one would
/// flatten every series but the largest into a straight line.
///
/// A flat series renders mid-height rather than at the floor. All-`▁` reads as
/// "bottomed out" when it actually means "did not move", which for a
/// deterministic metric is the best possible outcome and should not look like
/// the worst.
pub fn sparkline(values: &[f64]) -> String {
    if values.len() < 2 {
        return String::new();
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if max <= min {
        return BARS[BARS.len() / 2].to_string().repeat(values.len());
    }
    values
        .iter()
        .map(|v| {
            let t = (v - min) / (max - min);
            let i = (t * (BARS.len() - 1) as f64).round() as usize;
            BARS[i.min(BARS.len() - 1)]
        })
        .collect()
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

/// A percentage for display, or `new` when there is no base to compare to.
fn signed_pct(p: Option<f64>) -> String {
    match p {
        Some(p) => format!("{}{:.2}%", if p >= 0.0 { "+" } else { "" }, p),
        None => "new".to_string(),
    }
}

/// Render a comparison as markdown, suitable for a PR comment or a terminal.
///
/// One format rather than two. A markdown table is readable unrendered, so a
/// second plain-text renderer would be a second thing to keep correct for no
/// gain.
/// The line naming tak, appended unless the `credit` setting is off.
///
/// A report that turns up in someone's pull request should say what put it
/// there: a reader who has never seen tak needs a way to find out, and a
/// maintainer deciding whether to keep the comment needs to know what to turn
/// off. Small, last, and one line — advertising that gets in the way of the
/// numbers would be its own argument for removing it.
const CREDIT: &str = "\n<sub>Measured by [tak](https://github.com/jdx/tak) — instruction-counted \
     CLI benchmarks, stored in this repository's git notes.</sub>\n";

pub fn markdown(c: &Comparison, trend: &Trend, gate_pct: f64, credit: bool) -> String {
    let mut out = String::new();

    if c.is_empty() {
        // No early return. `added` and `removed` are often *most* interesting
        // here — a runner migration produces exactly this state, and the useful
        // half of the report is which benchmarks stopped being comparable.
        out.push_str(
            "**Nothing was compared, and so nothing was gated.** No series \
             appears on both sides: either the base has no measurements \
             recorded, or the two were measured on different runner classes, \
             which are deliberately not comparable — counts shift between \
             machine types by more than a real regression does.\n",
        );
    } else {
        out.push_str(&table(c, trend, gate_pct));
    }

    out.push_str(&outliers(c));
    out.push_str(
        "\n<sub>Only instruction counts gate. Wall clock is shown for context — \
         on identical hardware it moves 4-20% run to run.</sub>\n",
    );
    if credit {
        out.push_str(CREDIT);
    }
    out
}

/// The comparison table and its verdict.
fn table(c: &Comparison, trend: &Trend, gate_pct: f64) -> String {
    let mut out = String::new();
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

    let any_trend = series
        .keys()
        .any(|k| trend.get(k).is_some_and(|v| v.len() > 1));
    if any_trend {
        out.push_str("| benchmark | trend | instructions | Δ | wall (min) | Δ |\n");
        out.push_str("|---|---|---:|---:|---:|---:|\n");
    } else {
        out.push_str("| benchmark | instructions | Δ | wall (min) | Δ |\n");
        out.push_str("|---|---:|---:|---:|---:|\n");
    }
    for (key, (ins, wall)) in &series {
        let (bench, tool, _runner) = key;
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
        if any_trend {
            let spark = trend
                .get(key)
                .map(|v| sparkline(v))
                .filter(|s| !s.is_empty())
                .map(|s| format!("`{s}`"))
                .unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "| {name} | {spark} | {ins_cell} | {ins_delta} | {wall_cell} | {wall_delta} |\n"
            ));
        } else {
            out.push_str(&format!(
                "| {name} | {ins_cell} | {ins_delta} | {wall_cell} | {wall_delta} |\n"
            ));
        }
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

    out
}

/// How a series is named in prose: bench, plus the tool when it is not the
/// project itself, plus the runner.
///
/// Dropping the tool made two series that differ only by tool render
/// identically, so a report could say the same benchmark both started and
/// stopped gating and mean two different programs.
fn describe(key: &Key) -> String {
    let (bench, tool, runner) = key;
    if tool == "self" {
        format!("`{bench}` on `{runner}`")
    } else {
        format!("`{bench}` ({tool}) on `{runner}`")
    }
}

/// Series that exist on only one side. Always reported, including when nothing
/// overlapped — that is the case where they matter most.
fn outliers(c: &Comparison) -> String {
    let mut out = String::new();
    if !c.added.is_empty() {
        out.push_str(&format!(
            "\nNew, nothing to compare against: {}\n",
            c.added.iter().map(describe).collect::<Vec<_>>().join(", ")
        ));
    }
    if !c.removed.is_empty() {
        out.push_str(&format!(
            "\nMeasured on the base but not here — a benchmark that stops \
             running also stops gating: {}\n",
            c.removed
                .iter()
                .map(describe)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
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
        let ins = c.changes.iter().find(|c| c.metric == GATED_METRIC).unwrap();
        assert_eq!(ins.pct().unwrap().round(), -50.0);
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
        assert_eq!(wall.pct().unwrap().round(), 900.0);
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

        // The report has to say so. Asserting only on the struct let an early
        // return hide both lists from every reader of the actual output.
        let md = markdown(&c, &Trend::new(), 1.0, false);
        assert!(md.contains("nothing was gated"), "{md}");
        assert!(md.contains("gha-macos"), "the new runner is unnamed: {md}");
        assert!(md.contains("gha-linux"), "the old runner is unnamed: {md}");
    }

    /// A head with nothing recorded is the quietest possible failure: every
    /// benchmark stops gating at once and the gate still exits 0. It has to be
    /// loud in the report, and it has to name what vanished.
    #[test]
    fn a_head_with_no_measurements_names_what_vanished() {
        let c = compare(
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("b", "gha", 2.0, 2.0),
            ],
            &[],
        );
        let md = markdown(&c, &Trend::new(), 1.0, false);
        assert!(c.regressions(1.0).is_empty());
        assert!(md.contains("nothing was gated"), "{md}");
        assert!(md.contains("`a`") && md.contains("`b`"), "{md}");
        assert!(md.contains("stops gating"), "{md}");
    }

    /// The footnote about what gates belongs on every report, including the
    /// ones with no table.
    #[test]
    fn the_gating_caveat_survives_an_empty_comparison() {
        let md = markdown(&compare(&[], &[]), &Trend::new(), 1.0, false);
        assert!(md.contains("Only instruction counts gate"), "{md}");
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
        assert!(markdown(&c, &Trend::new(), 1.0, false).contains("stops gating"));
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
        assert!(markdown(&c, &Trend::new(), 1.0, false).contains("nothing was gated"));
    }

    /// A percentage of zero is undefined, and the gate used to read that as
    /// "no change" — so a base of zero disabled the gate for that benchmark
    /// entirely, whatever the head measured.
    #[test]
    fn a_rise_from_a_zero_base_still_gates() {
        let c = compare(
            &[rec("a", "gha", 0.0, 10.0)],
            &[rec("a", "gha", 5_000_000.0, 10.0)],
        );
        let ins = c.changes.iter().find(|c| c.metric == GATED_METRIC).unwrap();
        assert_eq!(ins.pct(), None, "no percentage exists against zero");
        assert_eq!(c.regressions(1.0).len(), 1, "it must still fail the gate");
        assert!(markdown(&c, &Trend::new(), 1.0, false).contains("new"));
    }

    /// Zero to zero is not a regression; there is nothing to report.
    #[test]
    fn zero_to_zero_is_not_a_regression() {
        let c = compare(&[rec("a", "gha", 0.0, 1.0)], &[rec("a", "gha", 0.0, 1.0)]);
        assert!(c.regressions(1.0).is_empty());
    }

    /// Two series that differ only by tool must not render identically, or the
    /// report can say the same benchmark both started and stopped gating while
    /// meaning two different programs.
    #[test]
    fn outliers_keep_their_tool() {
        let mut pnpm = rec("install", "gha", 1.0, 1.0);
        pnpm.tool = "pnpm".into();
        let c = compare(&[], &[rec("install", "gha", 1.0, 1.0), pnpm]);
        let md = markdown(&c, &Trend::new(), 1.0, false);
        assert!(md.contains("`install` on `gha`"), "{md}");
        assert!(md.contains("`install` (pnpm) on `gha`"), "{md}");
    }

    #[test]
    fn a_sparkline_spans_the_full_range() {
        let s = sparkline(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(s.chars().count(), 5);
        assert_eq!(s.chars().next().unwrap(), '▁');
        assert_eq!(s.chars().last().unwrap(), '█');
    }

    /// A deterministic metric that never moves is the best possible outcome.
    /// Drawing it at the floor would make it look like the worst.
    #[test]
    fn a_flat_series_sits_mid_height() {
        let s = sparkline(&[7.0, 7.0, 7.0]);
        assert_eq!(s, "▅▅▅");
    }

    /// One point is not a trend, and a single bar would imply a shape that was
    /// never measured.
    #[test]
    fn one_point_draws_nothing() {
        assert_eq!(sparkline(&[1.0]), "");
        assert_eq!(sparkline(&[]), "");
    }

    /// Each series is scaled to itself. A shared axis would flatten a 7M
    /// startup benchmark into a straight line beside a 100M install one.
    #[test]
    fn series_are_scaled_independently() {
        let small = sparkline(&[7_000_000.0, 7_100_000.0]);
        let large = sparkline(&[100_000_000.0, 101_000_000.0]);
        assert_eq!(small, large, "both rise by the same shape");
    }

    #[test]
    fn the_trend_column_appears_only_when_there_is_a_trend() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 1_000_000.0, 10.0)],
        );
        assert!(!markdown(&c, &Trend::new(), 1.0, false).contains("| trend |"));

        let mut trend = Trend::new();
        trend.insert(
            ("a".into(), "self".into(), "gha".into()),
            vec![1.0, 2.0, 3.0],
        );
        let md = markdown(&c, &trend, 1.0, false);
        assert!(md.contains("| trend |"), "{md}");
        assert!(md.contains('█'), "{md}");
    }

    #[test]
    fn thousands_separates() {
        assert_eq!(thousands(1234567.0), "1,234,567");
        assert_eq!(thousands(999.0), "999");
        assert_eq!(thousands(1000.0), "1,000");
    }
}
