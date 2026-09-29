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

use crate::config::SELF_TOOL;
use crate::record::{Record, is_builtin_metric};
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

    /// Has this risen beyond `gate`'s threshold?
    ///
    /// Both limits have to be crossed: the percentage and the absolute floor.
    /// The floor is for the benchmarks a percentage serves worst. On a
    /// 450k-instruction startup check, 1% is 4,500 instructions — a new
    /// dependency's relocations in the dynamic loader rather than anything the
    /// program did — while the same 1% of a 100M-instruction install is a
    /// million instructions of real work.
    ///
    /// From a zero base, any increase past the floor counts. There is no
    /// percentage that means anything against zero, and passing would be the
    /// one outcome that is certainly wrong.
    ///
    /// Whether the gate is enabled is not this function's question: a
    /// report-only series is still held to its threshold, so the report can say
    /// that it crossed one.
    pub fn exceeds(&self, gate: &Gate) -> bool {
        if self.metric != GATED_METRIC {
            return false;
        }
        // `as f64` rounds a floor above 2^53, far past any worth writing down.
        if self.head - self.base <= gate.min_delta as f64 {
            return false;
        }
        match self.pct() {
            Some(p) => p > gate.pct,
            None => self.head > 0.0,
        }
    }
}

/// When an instruction count has risen far enough to fail `tak compare`.
///
/// Policy, not measurement, so it is never recorded: it comes from the settings
/// and `tak.toml` of whoever runs the comparison. Stored with the records, a
/// threshold change would read as a change in what was measured, and two
/// writers with different gates would emit byte-different lines for the same
/// measurement, which `cat_sort_uniq` cannot collapse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gate {
    /// Percentage of the base a count may rise by.
    pub pct: f64,
    /// Instructions a count may rise by, whatever the percentage says.
    pub min_delta: u64,
    /// False for a report-only series: still held to its threshold and flagged
    /// when it crosses it, but never a reason to fail.
    pub enabled: bool,
}

impl Gate {
    /// An enabled gate, checked.
    pub fn new(pct: f64, min_delta: u64) -> anyhow::Result<Self> {
        check_pct(pct)?;
        Ok(Self {
            pct,
            min_delta,
            enabled: true,
        })
    }

    /// The threshold, short enough for a table cell: `5%`, `5%, floor 20,000`.
    fn threshold(&self) -> String {
        if self.min_delta == 0 {
            format!("{}%", self.pct)
        } else {
            format!("{}%, floor {}", self.pct, thousands(self.min_delta as f64))
        }
    }

    /// The threshold, and whether crossing it fails.
    fn describe(&self) -> String {
        if self.enabled {
            self.threshold()
        } else {
            format!("report only ({})", self.threshold())
        }
    }
}

/// Reject a gate percentage that cannot mean what it says.
///
/// NaN is the dangerous one: every comparison against it is false, so a gate of
/// NaN passes everything while looking set. A negative percentage fails a
/// benchmark that did not move, and infinity is `enabled = false` spelled in a
/// way the report cannot show.
pub fn check_pct(pct: f64) -> anyhow::Result<()> {
    if !pct.is_finite() || pct < 0.0 {
        anyhow::bail!("a gate percentage must be a finite number, 0 or more, not {pct}");
    }
    Ok(())
}

/// The gate for every series, as `tak.toml` declares it.
///
/// Looked up by benchmark and tool, never by runner. A gate is a statement
/// about a benchmark — how much of its count is the program's own work — and
/// that does not change with the machine it is measured on.
#[derive(Debug, Clone, PartialEq)]
pub struct Gates {
    /// The `[gate]` settings: what a series without its own gate is held to.
    pub global: Gate,
    /// Each declared benchmark's own gate, for a series whose tool the
    /// benchmark does not declare as a subject (any more).
    benches: BTreeMap<String, Gate>,
    /// Each declared (benchmark, subject), with every layer applied.
    series: BTreeMap<(String, String), Gate>,
}

impl Gates {
    /// The same gate for everything, as when there is no `tak.toml`.
    pub fn uniform(global: Gate) -> Self {
        Self {
            global,
            benches: BTreeMap::new(),
            series: BTreeMap::new(),
        }
    }

    pub fn set_bench(&mut self, bench: &str, gate: Gate) {
        self.benches.insert(bench.to_string(), gate);
    }

    pub fn set_series(&mut self, bench: &str, tool: &str, gate: Gate) {
        self.series
            .insert((bench.to_string(), tool.to_string()), gate);
    }

    /// The gate for one series: its own, else its benchmark's, else the global
    /// one.
    ///
    /// A benchmark in the notes but no longer in `tak.toml` gets the global
    /// gate rather than none. It is usually on the base side only, where
    /// nothing gates it anyway; when it is on both, the project has not said
    /// it is special, and the gate everything else gets is the honest default.
    pub fn get(&self, bench: &str, tool: &str) -> Gate {
        self.series
            .get(&(bench.to_string(), tool.to_string()))
            .or_else(|| self.benches.get(bench))
            .copied()
            .unwrap_or(self.global)
    }

    fn of(&self, c: &Change) -> Gate {
        self.get(&c.bench, &c.tool)
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
    /// Changes beyond an enabled gate: what `tak compare` fails on.
    pub fn regressions(&self, gates: &Gates) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|c| {
                let gate = gates.of(c);
                gate.enabled && c.exceeds(&gate)
            })
            .collect()
    }

    /// Changes beyond a report-only gate: flagged, never failed on.
    pub fn reported(&self, gates: &Gates) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|c| {
                let gate = gates.of(c);
                !gate.enabled && c.exceeds(&gate)
            })
            .collect()
    }

    /// Whether every instruction count here is held to the global gate.
    ///
    /// When it is, the report and the error read exactly as they did before
    /// per-benchmark gates existed. Scripts grep them — tak's own perf-pr
    /// workflow among them — and a project that never wrote a per-benchmark
    /// gate should not have its output change under it.
    pub fn gated_uniformly(&self, gates: &Gates) -> bool {
        self.changes
            .iter()
            .filter(|c| c.metric == GATED_METRIC)
            .all(|c| gates.of(c) == gates.global)
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

/// Assemble a trend from trunk history plus the revision under comparison.
///
/// `walked` is oldest-first, each entry a commit and the records attached to it.
///
/// The head's point is appended only when the walk did not already contain it.
/// Appending unconditionally put it last even when it belongs in the middle —
/// `tak compare v1.33.0 --rev v1.30.0` compares against an *ancestor*, and the
/// line then ended on a value from earlier in the history while reading as
/// though time ran left to right.
pub fn build_trend(
    walked: &[(String, Vec<Record>)],
    head_sha: &str,
    head_records: &[Record],
) -> Trend {
    let mut trend = Trend::new();
    let mut head_in_history = false;
    for (sha, records) in walked {
        if sha == head_sha {
            head_in_history = true;
            // Its own measurements, in its own place in the timeline.
            add_point(&mut trend, head_records);
        } else {
            add_point(&mut trend, records);
        }
    }
    if !head_in_history {
        add_point(&mut trend, head_records);
    }
    trend
}

/// Append one point per series, taking the minimum across a commit's records.
///
/// The minimum, matching how `compare` folds duplicates. A commit can carry
/// several records for one series — a CI re-run, a retry — and taking each as
/// its own point let a noisy re-run put a spike in the trend that the table,
/// which reduces to the minimum, does not show.
fn add_point(trend: &mut Trend, records: &[Record]) {
    let mut lowest: BTreeMap<Key, f64> = BTreeMap::new();
    for r in records {
        if let Some(v) = r.metrics.get(GATED_METRIC) {
            lowest
                .entry((r.bench.clone(), r.tool.clone(), r.runner.clone()))
                .and_modify(|e| {
                    if v < e {
                        *e = *v;
                    }
                })
                .or_insert(*v);
        }
    }
    for (key, value) in lowest {
        trend.entry(key).or_default().push(value);
    }
}

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

pub fn markdown(c: &Comparison, trend: &Trend, gates: &Gates, credit: bool) -> String {
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
        out.push_str(&table(c, trend, gates));
        out.push_str(&custom_table(c));
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

/// How a series is named in a table row or a verdict: the bench, plus the tool
/// when it is not the project itself.
fn name(bench: &str, tool: &str) -> String {
    if tool == SELF_TOOL {
        bench.to_string()
    } else {
        format!("{bench} ({tool})")
    }
}

/// The comparison table and its verdict.
fn table(c: &Comparison, trend: &Trend, gates: &Gates) -> String {
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
    // Only when some row is held to something other than the global gate. A
    // column that says `1%` on every row is noise, and it would change the
    // report of every project that never wrote a per-benchmark gate.
    let uniform = c.gated_uniformly(gates);

    let mut header = vec![("benchmark", "---")];
    if any_trend {
        header.push(("trend", "---"));
    }
    header.extend([("instructions", "---:"), ("Δ", "---:")]);
    if !uniform {
        header.push(("gate", "---"));
    }
    header.extend([("wall (min)", "---:"), ("Δ", "---:")]);
    let row = |cells: &[String]| format!("| {} |\n", cells.join(" | "));
    out.push_str(&row(&header
        .iter()
        .map(|(h, _)| h.to_string())
        .collect::<Vec<_>>()));
    out.push_str(&format!(
        "|{}|\n",
        header.iter().map(|(_, a)| *a).collect::<Vec<_>>().join("|")
    ));

    for (key, (ins, wall)) in &series {
        let (bench, tool, _runner) = key;
        let gate = gates.get(bench, tool);
        let mut cells = vec![name(bench, tool)];
        if any_trend {
            cells.push(
                trend
                    .get(key)
                    .map(|v| sparkline(v))
                    .filter(|s| !s.is_empty())
                    .map(|s| format!("`{s}`"))
                    .unwrap_or_else(|| "—".into()),
            );
        }
        match ins {
            Some(ch) => {
                // A report-only row that crossed its threshold says so in
                // words: the warning sign means "this fails", and a row that
                // cannot fail should not wear it.
                let flag = match (ch.exceeds(&gate), gate.enabled) {
                    (true, true) => " ⚠️",
                    (true, false) => " (not gated)",
                    (false, _) => "",
                };
                cells.push(format!("{} → {}", thousands(ch.base), thousands(ch.head)));
                cells.push(format!("**{}**{flag}", signed_pct(ch.pct())));
            }
            None => cells.extend(["—".into(), "—".into()]),
        }
        if !uniform {
            // A row with no instruction count has nothing a gate applies to.
            cells.push(if ins.is_some() {
                gate.describe()
            } else {
                "—".into()
            });
        }
        match wall {
            Some(ch) => {
                cells.push(format!("{:.2} → {:.2}ms", ch.base, ch.head));
                cells.push(signed_pct(ch.pct()));
            }
            None => cells.extend(["—".into(), "—".into()]),
        }
        out.push_str(&row(&cells));
    }

    out.push('\n');
    out.push_str(&verdict(c, gates, uniform));
    out
}

/// The lines under the table that say what failed and what only rose.
///
/// With every row at the global gate, the wording is exactly what it was
/// before per-benchmark gates existed, because scripts grep it: tak's own
/// perf-pr workflow reads `benchmark(s) above the N% gate` to decide whether
/// counts rose.
fn verdict(c: &Comparison, gates: &Gates, uniform: bool) -> String {
    let mut out = String::new();
    let global = gates.global;
    let listed = |changes: &[&Change], annotate: bool| {
        changes
            .iter()
            .map(|ch| {
                let mut s = format!("`{}` {}", name(&ch.bench, &ch.tool), signed_pct(ch.pct()));
                if annotate {
                    s.push_str(&format!(" (gate {})", gates.of(ch).threshold()));
                }
                s
            })
            .collect::<Vec<_>>()
            .join(", ")
    };

    let regressions = c.regressions(gates);
    if uniform {
        // Named on both verdicts. Without it, a failing report can show one 2%
        // rise failing beside another passing and give no reason; the floor is
        // the reason. After the `N% gate` wording, which scripts match on.
        let floor = if global.min_delta == 0 {
            String::new()
        } else {
            format!(
                " (rises of {} instructions or fewer are not counted)",
                thousands(global.min_delta as f64)
            )
        };
        if regressions.is_empty() {
            out.push_str(&format!(
                "No instruction-count regression above {}%{floor}.\n",
                global.pct,
            ));
        } else {
            out.push_str(&format!(
                "**{} benchmark(s) above the {}% gate{floor}:** {}\n",
                regressions.len(),
                global.pct,
                listed(&regressions, false)
            ));
        }
        return out;
    }

    let any_gated = c
        .changes
        .iter()
        .any(|ch| ch.metric == GATED_METRIC && gates.of(ch).enabled);
    if !any_gated {
        // "No gated benchmark rose" is true of a table with none, and reads
        // as a pass. Saying there was nothing to fail is the honest verdict.
        out.push_str("Every benchmark here is report-only, so none can fail the gate.\n");
    } else if regressions.is_empty() {
        out.push_str("No gated benchmark rose beyond its gate.\n");
    } else {
        out.push_str(&format!(
            "**{} benchmark(s) above their gate:** {}\n",
            regressions.len(),
            listed(&regressions, true)
        ));
    }
    let reported = c.reported(gates);
    if !reported.is_empty() {
        out.push_str(&format!(
            "\n**{} report-only benchmark(s) above their gate, not failing:** {}\n",
            reported.len(),
            listed(&reported, true)
        ));
    }
    out
}

/// Custom metrics — anything a project declared in `tak.toml` rather than
/// something tak measured itself — as a table of their own, or nothing when
/// there are none.
///
/// Separate from the main table rather than more columns on it: the set of
/// custom metrics differs from project to project and from benchmark to
/// benchmark, so columns would be mostly empty, and a project without any
/// keeps exactly the table it had. One row per series and metric, in the
/// order `compare` produced them, which groups a series' metrics together.
///
/// Never flagged: nothing here gates, whatever it did. A declared metric is a
/// number from a file or a script tak knows nothing about, and nothing has
/// shown it to be as repeatable as an instruction count.
fn custom_table(c: &Comparison) -> String {
    let rows: Vec<&Change> = c
        .changes
        .iter()
        .filter(|ch| !is_builtin_metric(&ch.metric))
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n| benchmark | metric | value | Δ |\n|---|---|---:|---:|\n");
    for ch in rows {
        let name = if ch.tool == "self" {
            ch.bench.clone()
        } else {
            format!("{} ({})", ch.bench, ch.tool)
        };
        out.push_str(&format!(
            "| {name} | {} | {} → {} | {} |\n",
            ch.metric,
            plain(ch.base),
            plain(ch.head),
            signed_pct(ch.pct())
        ));
    }
    out.push_str(
        "\n<sub>Metrics declared in tak.toml are reported, never gated. As for every \
         metric, lower is taken as better.</sub>\n",
    );
    out
}

/// A custom metric's value: separated like a count when it is whole, which
/// sizes and counts are, and as written otherwise. `abs`, unlike when tak
/// prints its own measurement: a note may hold a line from any writer.
fn plain(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        thousands(v)
    } else {
        v.to_string()
    }
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

    fn key(bench: &str) -> Key {
        (bench.to_string(), "self".to_string(), "gha".to_string())
    }

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

    /// One global gate at `pct`, with no floor and no per-benchmark gates.
    fn g(pct: f64) -> Gates {
        Gates::uniform(Gate::new(pct, 0).unwrap())
    }

    fn gate(pct: f64, min_delta: u64, enabled: bool) -> Gate {
        Gate {
            pct,
            min_delta,
            enabled,
        }
    }

    /// `startup` at 450k instructions and `install` at 100M, both rising
    /// 2%: the pair one global percentage cannot serve.
    fn startup_and_install() -> Comparison {
        compare(
            &[
                rec("startup", "gha", 450_000.0, 1.0),
                rec("install", "gha", 100_000_000.0, 100.0),
            ],
            &[
                rec("startup", "gha", 459_000.0, 1.0),
                rec("install", "gha", 102_000_000.0, 100.0),
            ],
        )
    }

    fn benches(c: &[&Change]) -> Vec<String> {
        c.iter().map(|c| c.bench.clone()).collect()
    }

    /// Every combination of percentage and floor that decides a verdict, on
    /// a rise of 2% and 20,000 instructions from a 1M base.
    #[test]
    fn a_regression_crosses_both_the_percentage_and_the_floor() {
        let rise = Change {
            bench: "a".into(),
            tool: SELF_TOOL.into(),
            runner: "gha".into(),
            metric: GATED_METRIC.into(),
            base: 1_000_000.0,
            head: 1_020_000.0,
        };
        let cases = [
            // (pct, min_delta, exceeds)
            (1.0, 0, true),       // past the percentage, no floor
            (5.0, 0, false),      // under the percentage
            (1.0, 10_000, true),  // past both
            (1.0, 20_000, false), // exactly at the floor is not past it
            (1.0, 50_000, false), // past the percentage, under the floor
            (5.0, 10_000, false), // past the floor, under the percentage
            (2.0, 0, false),      // exactly at the percentage is not past it
            (0.0, 0, true),       // a zero gate fails on any rise
        ];
        for (pct, min_delta, want) in cases {
            assert_eq!(
                rise.exceeds(&gate(pct, min_delta, true)),
                want,
                "pct {pct}, min_delta {min_delta}"
            );
            // Enabled or not, the threshold is the same one.
            assert_eq!(rise.exceeds(&gate(pct, min_delta, false)), want);
        }

        let fall = Change {
            head: 500_000.0,
            ..rise.clone()
        };
        assert!(
            !fall.exceeds(&gate(0.0, 0, true)),
            "an improvement never gates"
        );
        let wall = Change {
            metric: WALL_METRIC.into(),
            ..rise
        };
        assert!(!wall.exceeds(&gate(0.0, 0, true)), "wall clock never gates");
    }

    /// The floor holds from a zero base too: without it, zero to anything
    /// fails; with it, zero to less than the floor does not.
    #[test]
    fn a_zero_base_is_still_held_to_the_floor() {
        let c = compare(
            &[rec("a", "gha", 0.0, 1.0)],
            &[rec("a", "gha", 5_000.0, 1.0)],
        );
        assert_eq!(c.regressions(&g(1.0)).len(), 1);
        let floored = Gates::uniform(Gate::new(1.0, 10_000).unwrap());
        assert!(c.regressions(&floored).is_empty());
    }

    /// A gate of NaN passes everything, since every comparison against it is
    /// false. It must not be constructible from a setting.
    #[test]
    fn a_gate_percentage_must_be_finite_and_not_negative() {
        assert!(Gate::new(0.0, 0).is_ok());
        assert!(Gate::new(250.0, 0).is_ok());
        for bad in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = Gate::new(bad, 0).unwrap_err();
            assert!(format!("{err}").contains("gate percentage"), "{err}");
        }
    }

    #[test]
    fn a_series_gate_beats_its_benchmarks_which_beats_the_global_one() {
        let mut gates = g(1.0);
        gates.set_bench("install", gate(5.0, 0, true));
        gates.set_series("install", "pnpm", gate(10.0, 0, false));
        assert_eq!(gates.get("install", "pnpm"), gate(10.0, 0, false));
        assert_eq!(
            gates.get("install", "npm"),
            gate(5.0, 0, true),
            "a subject the benchmark no longer declares gets the benchmark's"
        );
        assert_eq!(
            gates.get("gone", SELF_TOOL),
            gate(1.0, 0, true),
            "a benchmark no longer in tak.toml gets the global gate"
        );
    }

    /// The motivating case: a loose gate on the small benchmark lets its 2%
    /// through while the large one is still held to 1%.
    #[test]
    fn a_per_benchmark_gate_decides_its_own_series_only() {
        let c = startup_and_install();
        assert_eq!(benches(&c.regressions(&g(1.0))), ["install", "startup"]);

        let mut gates = g(1.0);
        gates.set_series("startup", SELF_TOOL, gate(5.0, 0, true));
        assert_eq!(benches(&c.regressions(&gates)), ["install"]);

        // A tighter one works too: nothing says a per-benchmark gate is looser.
        let mut gates = g(5.0);
        gates.set_series("install", SELF_TOOL, gate(1.0, 0, true));
        assert_eq!(benches(&c.regressions(&gates)), ["install"]);
    }

    /// Two subjects of one benchmark are two series, and can be held to
    /// different gates.
    #[test]
    fn subjects_of_one_benchmark_take_their_own_gates() {
        let with_tool = |tool: &str, ins: f64| Record {
            tool: tool.into(),
            ..rec("install", "gha", ins, 1.0)
        };
        let c = compare(
            &[
                with_tool("mine", 1_000_000.0),
                with_tool("theirs", 1_000_000.0),
            ],
            &[
                with_tool("mine", 1_100_000.0),
                with_tool("theirs", 1_100_000.0),
            ],
        );
        let mut gates = g(1.0);
        gates.set_series("install", "theirs", gate(1.0, 0, false));
        let tools = |v: Vec<&Change>| v.iter().map(|c| c.tool.clone()).collect::<Vec<_>>();
        assert_eq!(tools(c.regressions(&gates)), ["mine"]);
        assert_eq!(tools(c.reported(&gates)), ["theirs"]);
    }

    /// A report-only series is measured against its threshold and reported
    /// when it crosses it, and never fails.
    #[test]
    fn a_report_only_series_is_flagged_and_never_fails() {
        let c = startup_and_install();
        let mut gates = g(1.0);
        gates.set_series("startup", SELF_TOOL, gate(1.0, 0, false));
        assert_eq!(benches(&c.regressions(&gates)), ["install"]);
        assert_eq!(benches(&c.reported(&gates)), ["startup"]);

        let md = markdown(&c, &Trend::new(), &gates, false);
        assert!(md.contains("| gate |"), "{md}");
        assert!(md.contains("report only (1%)"), "{md}");
        assert!(md.contains("**+2.00%** (not gated)"), "{md}");
        assert!(
            md.contains("**1 benchmark(s) above their gate:** `install` +2.00% (gate 1%)"),
            "{md}"
        );
        assert!(
            md.contains(
                "**1 report-only benchmark(s) above their gate, not failing:** \
                 `startup` +2.00% (gate 1%)"
            ),
            "{md}"
        );
    }

    /// A project that never wrote a per-benchmark gate must get the report it
    /// got before they existed, byte for byte: scripts grep the verdict.
    #[test]
    fn uniform_gates_render_the_report_unchanged() {
        let md = markdown(&startup_and_install(), &Trend::new(), &g(1.0), false);
        assert_eq!(
            md,
            "| benchmark | instructions | Δ | wall (min) | Δ |\n\
             |---|---:|---:|---:|---:|\n\
             | install | 100,000,000 → 102,000,000 | **+2.00%** ⚠️ | 100.00 → 100.00ms | +0.00% |\n\
             | startup | 450,000 → 459,000 | **+2.00%** ⚠️ | 1.00 → 1.00ms | +0.00% |\n\
             \n\
             **2 benchmark(s) above the 1% gate:** `install` +2.00%, `startup` +2.00%\n\
             \n\
             <sub>Only instruction counts gate. Wall clock is shown for context — \
             on identical hardware it moves 4-20% run to run.</sub>\n"
        );

        // A per-benchmark gate equal to the global one is not a difference.
        let mut same = g(1.0);
        same.set_series("startup", SELF_TOOL, gate(1.0, 0, true));
        assert_eq!(
            markdown(&startup_and_install(), &Trend::new(), &same, false),
            md
        );
    }

    #[test]
    fn a_differing_gate_adds_a_column_with_every_rows_gate() {
        let mut gates = g(1.0);
        gates.set_series("startup", SELF_TOOL, gate(5.0, 20_000, true));
        let md = markdown(&startup_and_install(), &Trend::new(), &gates, false);
        assert!(
            md.contains("| benchmark | instructions | Δ | gate | wall (min) | Δ |\n|---|---:|---:|---|---:|---:|\n"),
            "{md}"
        );
        assert!(md.contains("| **+2.00%** | 5%, floor 20,000 |"), "{md}");
        assert!(md.contains("| **+2.00%** ⚠️ | 1% |"), "{md}");
        assert!(
            md.contains("above their gate:** `install` +2.00% (gate 1%)\n"),
            "{md}"
        );
    }

    #[test]
    fn nothing_over_any_gate_says_so() {
        let mut gates = g(5.0);
        gates.set_series("startup", SELF_TOOL, gate(10.0, 0, true));
        let md = markdown(&startup_and_install(), &Trend::new(), &gates, false);
        assert!(
            md.contains("No gated benchmark rose beyond its gate."),
            "{md}"
        );
        assert!(!md.contains("report-only"), "{md}");
    }

    /// A table with nothing gated must not read as a pass.
    #[test]
    fn a_table_with_nothing_gated_says_so() {
        let mut gates = g(1.0);
        gates.set_series("startup", SELF_TOOL, gate(1.0, 0, false));
        gates.set_series("install", SELF_TOOL, gate(50.0, 0, false));
        let md = markdown(&startup_and_install(), &Trend::new(), &gates, false);
        assert!(
            md.contains("Every benchmark here is report-only, so none can fail the gate."),
            "{md}"
        );
        assert!(!md.contains("No gated benchmark"), "{md}");
        assert!(
            md.contains("**1 report-only benchmark(s) above their gate, not failing:** `startup`"),
            "{md}"
        );
    }

    /// A global floor has to be visible in the one line that says nothing
    /// failed, or a rise under it reads as no rise at all.
    #[test]
    fn a_global_floor_is_named_in_the_verdict() {
        let gates = Gates::uniform(Gate::new(1.0, 20_000).unwrap());
        let md = markdown(&startup_and_install(), &Trend::new(), &gates, false);
        assert!(
            !md.contains("| gate |"),
            "the global gate needs no column: {md}"
        );
        assert!(
            md.contains(
                "**1 benchmark(s) above the 1% gate \
                 (rises of 20,000 instructions or fewer are not counted):** `install` +2.00%"
            ),
            "{md}"
        );
        let c = compare(
            &[rec("a", "gha", 1_000.0, 1.0)],
            &[rec("a", "gha", 1_500.0, 1.0)],
        );
        let md = markdown(&c, &Trend::new(), &gates, false);
        assert!(
            md.contains(
                "No instruction-count regression above 1% \
                 (rises of 20,000 instructions or fewer are not counted)."
            ),
            "{md}"
        );
    }

    #[test]
    fn a_rise_beyond_the_gate_is_a_regression() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 1_020_000.0, 10.0)],
        );
        assert_eq!(c.regressions(&g(1.0)).len(), 1, "2% should trip a 1% gate");
        assert!(c.regressions(&g(5.0)).is_empty(), "2% should not trip 5%");
    }

    #[test]
    fn an_improvement_never_gates() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 500_000.0, 10.0)],
        );
        assert!(c.regressions(&g(1.0)).is_empty());
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
        assert!(c.regressions(&g(0.001)).is_empty());
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
        assert!(c.regressions(&g(1.0)).is_empty());

        // The report has to say so. Asserting only on the struct let an early
        // return hide both lists from every reader of the actual output.
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
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
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(c.regressions(&g(1.0)).is_empty());
        assert!(md.contains("nothing was gated"), "{md}");
        assert!(md.contains("`a`") && md.contains("`b`"), "{md}");
        assert!(md.contains("stops gating"), "{md}");
    }

    /// The footnote about what gates belongs on every report, including the
    /// ones with no table.
    #[test]
    fn the_gating_caveat_survives_an_empty_comparison() {
        let md = markdown(&compare(&[], &[]), &Trend::new(), &g(1.0), false);
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
        assert!(c.regressions(&g(1.0)).is_empty());
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
        assert!(markdown(&c, &Trend::new(), &g(1.0), false).contains("stops gating"));
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
        assert!(
            c.regressions(&g(1.0)).is_empty(),
            "the clean sample should win"
        );
    }

    #[test]
    fn nothing_in_common_says_so_rather_than_passing_quietly() {
        let c = compare(&[], &[rec("a", "gha", 1.0, 1.0)]);
        assert!(c.is_empty());
        assert!(markdown(&c, &Trend::new(), &g(1.0), false).contains("nothing was gated"));
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
        assert_eq!(
            c.regressions(&g(1.0)).len(),
            1,
            "it must still fail the gate"
        );
        assert!(markdown(&c, &Trend::new(), &g(1.0), false).contains("new"));
    }

    /// Zero to zero is not a regression; there is nothing to report.
    #[test]
    fn zero_to_zero_is_not_a_regression() {
        let c = compare(&[rec("a", "gha", 0.0, 1.0)], &[rec("a", "gha", 0.0, 1.0)]);
        assert!(c.regressions(&g(1.0)).is_empty());
    }

    /// Two series that differ only by tool must not render identically, or the
    /// report can say the same benchmark both started and stopped gating while
    /// meaning two different programs.
    #[test]
    fn outliers_keep_their_tool() {
        let mut pnpm = rec("install", "gha", 1.0, 1.0);
        pnpm.tool = "pnpm".into();
        let c = compare(&[], &[rec("install", "gha", 1.0, 1.0), pnpm]);
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(md.contains("`install` on `gha`"), "{md}");
        assert!(md.contains("`install` (pnpm) on `gha`"), "{md}");
    }

    /// The common case: the revision under comparison is a branch commit, so
    /// it is not in trunk history and its point ends the line.
    #[test]
    fn a_branch_head_is_appended_last() {
        let walked = vec![
            ("c1".to_string(), vec![rec("a", "gha", 10.0, 1.0)]),
            ("c2".to_string(), vec![rec("a", "gha", 20.0, 1.0)]),
        ];
        let t = build_trend(&walked, "branch", &[rec("a", "gha", 30.0, 1.0)]);
        assert_eq!(t[&key("a")], vec![10.0, 20.0, 30.0]);
    }

    /// Comparing against an ancestor — `tak compare v1.33.0 --rev v1.30.0` —
    /// puts the head *inside* the walked history. Appending it at the end as
    /// well would draw a line that reads as time and is not in time order.
    #[test]
    fn a_head_inside_history_stays_in_its_place() {
        let walked = vec![
            ("c1".to_string(), vec![rec("a", "gha", 10.0, 1.0)]),
            ("head".to_string(), vec![rec("a", "gha", 20.0, 1.0)]),
            ("c3".to_string(), vec![rec("a", "gha", 30.0, 1.0)]),
        ];
        let t = build_trend(&walked, "head", &[rec("a", "gha", 99.0, 1.0)]);
        assert_eq!(
            t[&key("a")],
            vec![10.0, 99.0, 30.0],
            "the head's own measurement belongs where the head is, once"
        );
    }

    /// Duplicate records within one commit collapse to the minimum, the same
    /// rule the table uses — otherwise the two halves of a report disagree.
    #[test]
    fn a_commit_contributes_one_point() {
        let walked = vec![(
            "c1".to_string(),
            vec![rec("a", "gha", 50.0, 1.0), rec("a", "gha", 10.0, 1.0)],
        )];
        let t = build_trend(&walked, "branch", &[]);
        assert_eq!(t[&key("a")], vec![10.0]);
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
        assert!(!markdown(&c, &Trend::new(), &g(1.0), false).contains("| trend |"));

        let mut trend = Trend::new();
        trend.insert(
            ("a".into(), "self".into(), "gha".into()),
            vec![1.0, 2.0, 3.0],
        );
        let md = markdown(&c, &trend, &g(1.0), false);
        assert!(md.contains("| trend |"), "{md}");
        assert!(md.contains('█'), "{md}");
    }

    /// Declared metrics get a table of their own and never gate, however far
    /// they move; the main table is exactly what it was without them.
    #[test]
    fn custom_metrics_are_a_separate_table_and_never_gate() {
        let with = |bytes: f64, kb: f64| {
            let mut r = rec("a", "gha", 1_000_000.0, 10.0);
            r.metrics.insert("binary_bytes".into(), bytes);
            r.metrics.insert("bundle_kb".into(), kb);
            r
        };
        let c = compare(&[with(4_000_000.0, 10.5)], &[with(8_000_000.0, 12.25)]);
        assert!(
            c.regressions(&g(0.0)).is_empty(),
            "a doubled binary must not fail the gate"
        );
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(md.contains("| benchmark | metric | value | Δ |"), "{md}");
        assert!(
            md.contains("| a | binary_bytes | 4,000,000 → 8,000,000 | +100.00% |"),
            "{md}"
        );
        assert!(
            md.contains("| a | bundle_kb | 10.5 → 12.25 | +16.67% |"),
            "{md}"
        );
        assert!(md.contains("No instruction-count regression"), "{md}");
        assert!(md.contains("never gated"), "{md}");

        let plain = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 1_000_000.0, 10.0)],
        );
        let md = markdown(&plain, &Trend::new(), &g(1.0), false);
        assert!(!md.contains("| metric |"), "{md}");
        assert!(!md.contains("tak.toml"), "{md}");
        // The main table has the same columns with or without them.
        let header = |md: &str| md.lines().next().unwrap().to_string();
        assert_eq!(
            header(&markdown(&c, &Trend::new(), &g(1.0), false)),
            header(&md)
        );
    }

    /// Timing statistics other than the minimum are recorded but were never
    /// shown; they must not start appearing as if they were declared.
    #[test]
    fn built_in_metrics_stay_out_of_the_custom_table() {
        let with = |p50: f64| {
            let mut r = rec("a", "gha", 1_000_000.0, 10.0);
            r.metrics.insert("wall_p50_ms".into(), p50);
            r
        };
        let md = markdown(
            &compare(&[with(11.0)], &[with(12.0)]),
            &Trend::new(),
            &g(1.0),
            false,
        );
        assert!(!md.contains("wall_p50_ms"), "{md}");
    }

    #[test]
    fn thousands_separates() {
        assert_eq!(thousands(1234567.0), "1,234,567");
        assert_eq!(thousands(999.0), "999");
        assert_eq!(thousands(1000.0), "1,000");
    }
}
