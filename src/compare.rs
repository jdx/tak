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

use crate::accept::Acceptances;
use crate::config::SELF_TOOL;
use crate::record::Record;
use std::collections::{BTreeMap, BTreeSet};

/// The only metric a gate may fire on.
pub const GATED_METRIC: &str = "instructions";

/// The timing metric shown alongside it, for context only.
pub(crate) const WALL_METRIC: &str = "wall_min_ms";

/// Heap-allocation metrics, in the order their columns appear. Recorded only
/// by subjects that opt in, so they get a table of their own rather than
/// columns every report would carry empty.
const ALLOC_METRICS: [(&str, &str); 3] = [
    ("alloc_blocks", "allocations"),
    ("alloc_bytes", "bytes allocated"),
    ("alloc_peak_bytes", "peak heap"),
];

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
    pub(crate) fn threshold(&self) -> String {
        if self.min_delta == 0 {
            format!("{}%", self.pct)
        } else {
            format!("{}%, floor {}", self.pct, thousands(self.min_delta as f64))
        }
    }

    /// The threshold, and whether crossing it fails.
    pub(crate) fn describe(&self) -> String {
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

    /// What these gates hold differently from `base`, by effective value: the
    /// global gate as `[gate]`, then each series either side declares or
    /// `measured` names whose resolved gate differs.
    ///
    /// Effective rather than structural. Declaring a benchmark with no `gate`
    /// adds it to the maps at the global gate, which is what an undeclared
    /// series gets anyway; reporting that as a change would tell a pull
    /// request that only added a benchmark its gate takes effect later. A
    /// series that merely follows `[gate]` on both sides is covered by
    /// `[gate]` and not named again.
    ///
    /// Each entry is a code span, escaped by [`code`] like every other name in
    /// the report: a benchmark name comes from the change under review, and one
    /// holding a newline must not start a line of its own.
    pub fn changes_from(&self, base: &Gates, measured: &BTreeSet<(String, String)>) -> Vec<String> {
        let mut out = Vec::new();
        if self.global != base.global {
            out.push(code("[gate]"));
        }
        let follows = |g: &Gates, gate: Gate| gate == g.global;
        let mut named = BTreeSet::new();
        // A benchmark's own gate is what its undeclared subjects fall back to.
        for bench in self.benches.keys().chain(base.benches.keys()) {
            let of = |g: &Gates| g.benches.get(bench).copied().unwrap_or(g.global);
            let (mine, theirs) = (of(self), of(base));
            if mine != theirs && !(follows(self, mine) && follows(base, theirs)) {
                named.insert(bench.clone());
            }
        }
        let series = self
            .series
            .keys()
            .chain(base.series.keys())
            .chain(measured.iter());
        for (bench, tool) in series {
            let (mine, theirs) = (self.get(bench, tool), base.get(bench, tool));
            if mine != theirs && !(follows(self, mine) && follows(base, theirs)) {
                named.insert(name(bench, tool));
            }
        }
        out.extend(named.iter().map(|n| code(n)));
        out
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
    /// Benchmarks the change declared it regresses on purpose. Their
    /// regressions are still reported, and still counted by [`regressions`],
    /// but not by [`failures`] — the set the gate actually fails on.
    ///
    /// [`regressions`]: Comparison::regressions
    /// [`failures`]: Comparison::failures
    pub accepted: Acceptances,
    /// `Tak-Accept` trailers found in the range and deliberately not honoured,
    /// because `accept_trailers` is off. Kept only to say so in the report.
    pub ignored_trailers: Acceptances,
    /// One Markdown line on where the gate came from, when that is worth
    /// saying: the base had no `tak.toml`, or the head's would gate
    /// differently. Rendered below the verdict rather than above it, because
    /// scripts read the report's first line as its outcome.
    pub gate_source: Option<String>,
}

impl Comparison {
    /// Changes beyond an enabled gate, accepted or not.
    pub fn regressions(&self, gates: &Gates) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|c| {
                let gate = gates.of(c);
                gate.enabled && c.exceeds(&gate)
            })
            .collect()
    }

    /// What `tak compare` fails on: regressions no acceptance covers.
    ///
    /// Layered on [`regressions`](Comparison::regressions), so each series is
    /// still judged against its own effective gate, and an acceptance can only
    /// waive a change that would otherwise have failed. A report-only series
    /// never reaches this, so it never needs accepting.
    pub fn failures(&self, gates: &Gates) -> Vec<&Change> {
        self.regressions(gates)
            .into_iter()
            .filter(|c| !self.accepted.covers(&c.bench))
            .collect()
    }

    /// The regressions an acceptance waived.
    pub fn accepted_regressions(&self, gates: &Gates) -> Vec<&Change> {
        self.regressions(gates)
            .into_iter()
            .filter(|c| self.accepted.covers(&c.bench))
            .collect()
    }

    /// Attach what the change declared. Separate from [`compare`] because the
    /// numbers and the declaration come from different places — notes and
    /// commit messages — and a comparison is meaningful without either.
    pub fn with_accepted(mut self, accepted: Acceptances) -> Self {
        self.accepted = accepted;
        self
    }

    /// Record trailers that were present but not honoured. They never affect
    /// the gate; they exist so the report can explain why a trailer did nothing.
    pub fn with_ignored_trailers(mut self, ignored: Acceptances) -> Self {
        self.ignored_trailers = ignored;
        self
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
pub(crate) fn index(records: &[Record]) -> BTreeMap<(Key, String), f64> {
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
        accepted: Acceptances::default(),
        ignored_trailers: Acceptances::default(),
        gate_source: None,
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
pub(crate) fn thousands(v: f64) -> String {
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
pub(crate) fn signed_pct(p: Option<f64>) -> String {
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
pub(crate) const CREDIT: &str = "\n<sub>Measured by [tak](https://github.com/jdx/tak) — instruction-counted \
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
        out.push_str(&allocations(c));
    }

    if let Some(line) = &c.gate_source {
        out.push_str(&format!("\n{line}\n"));
    }
    out.push_str(&unused_acceptances(c, gates));
    out.push_str(&ignored_trailers(c));
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

/// A series' table cell: [`name`], plus the runner when `keys` holds the
/// same bench and tool on another runner. Without it the two rows read
/// identically, and nothing says which change was measured where.
///
/// Escaped like every other report site: control characters by [`name`]
/// and [`escape_control`], then the whole cell by [`cell`], since bench,
/// tool and runner names are free text — a runner class comes from the
/// environment — and a bare `|` in any of them would split the row.
fn row_name<'a>(key: &Key, keys: impl IntoIterator<Item = &'a Key>) -> String {
    let (bench, tool, runner) = key;
    let shared = keys
        .into_iter()
        .any(|(b, t, r)| b == bench && t == tool && r != runner);
    let text = if shared {
        format!("{} on {}", name(bench, tool), escape_control(runner))
    } else {
        name(bench, tool)
    };
    cell(&text)
}

/// How a series is named in a table row or a verdict: the bench, plus the tool
/// when it is not the project itself.
///
/// Control characters are escaped. `tak.toml` rejects them at load, but a
/// note written by an older tak can still hold one, and a newline in a table
/// cell or a verdict would start a report line of its own.
fn name(bench: &str, tool: &str) -> String {
    let bench = escape_control(bench);
    if tool == SELF_TOOL {
        bench.into_owned()
    } else {
        format!("{bench} ({})", escape_control(tool))
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
        let mut cells = vec![row_name(key, series.keys())];
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
                // cannot fail should not wear it. An accepted row is marked
                // too, so it cannot be read as a passing one by someone who
                // only scans the table.
                let flag = match (ch.exceeds(&gate), gate.enabled) {
                    (true, true) if c.accepted.covers(bench) => " (accepted)",
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
    let failures = c.failures(gates);
    let accepted = c.accepted_regressions(gates);
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
        } else if !failures.is_empty() {
            out.push_str(&format!(
                "**{} benchmark(s) above the {}% gate{floor}:** {}\n",
                failures.len(),
                global.pct,
                listed(&failures, false)
            ));
        }
        out.push_str(&accepted_line(
            c,
            &accepted,
            &format!("the {}% gate", global.pct),
            !failures.is_empty(),
            |_| String::new(),
        ));
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
    } else if !failures.is_empty() {
        out.push_str(&format!(
            "**{} benchmark(s) above their gate:** {}\n",
            failures.len(),
            listed(&failures, true)
        ));
    }
    out.push_str(&accepted_line(
        c,
        &accepted,
        "their gate",
        !failures.is_empty(),
        |ch| format!(" (gate {})", gates.of(ch).threshold()),
    ));
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

/// The heap-allocation table, or nothing when no series has allocation
/// metrics on both sides — so a project that never opted in sees the report
/// it always did.
///
/// Never flagged, whatever the change: allocations are reported beside the
/// gate, not part of it, until they have been shown to be as reproducible as
/// instruction counts across the programs people measure.
fn allocations(c: &Comparison) -> String {
    let mut series: BTreeMap<Key, BTreeMap<&str, &Change>> = BTreeMap::new();
    for change in &c.changes {
        if ALLOC_METRICS.iter().any(|(m, _)| *m == change.metric) {
            series
                .entry((
                    change.bench.clone(),
                    change.tool.clone(),
                    change.runner.clone(),
                ))
                .or_default()
                .insert(change.metric.as_str(), change);
        }
    }
    if series.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nHeap allocations, counted by DHAT; reported, not gated:\n\n");
    out.push_str("| benchmark |");
    for (_, title) in ALLOC_METRICS {
        out.push_str(&format!(" {title} | Δ |"));
    }
    out.push_str("\n|---|");
    out.push_str(&"---:|---:|".repeat(ALLOC_METRICS.len()));
    out.push('\n');
    for (key, metrics) in &series {
        out.push_str(&format!("| {} |", row_name(key, series.keys())));
        for (metric, _) in ALLOC_METRICS {
            match metrics.get(metric) {
                Some(ch) => out.push_str(&format!(
                    " {} → {} | {} |",
                    thousands(ch.base),
                    thousands(ch.head),
                    count_delta(ch)
                )),
                None => out.push_str(" — | — |"),
            }
        }
        out.push('\n');
    }
    out
}

/// The accepted regressions, on their own line in bold, each with its runner,
/// its gate when `gate_of` names one, and where the acceptance came from.
///
/// Its own line because an acceptance is an override of the gate, and the
/// point of scoping it is that it stays as visible as the regression would
/// have been. `above {which}` keeps the wording the failure line uses, so a
/// script matching `(s) above the N% gate` or `above their gate` sees an
/// accepted rise as a rise. The runner is named because one benchmark
/// accepted on two runner classes is two entries that would otherwise read
/// identically.
fn accepted_line(
    c: &Comparison,
    accepted: &[&Change],
    which: &str,
    after_failures: bool,
    gate_of: impl Fn(&Change) -> String,
) -> String {
    if accepted.is_empty() {
        return String::new();
    }
    format!(
        "{}**{} accepted regression(s) above {which}, not failing it:** {}\n",
        if after_failures { "\n" } else { "" },
        accepted.len(),
        accepted
            .iter()
            .map(|ch| format!(
                "{} on {} {}{} ({})",
                code(&name(&ch.bench, &ch.tool)),
                code(&ch.runner),
                signed_pct(ch.pct()),
                gate_of(ch),
                c.accepted.describe(&ch.bench)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// `text` as a Markdown code span, whatever it contains.
///
/// Acceptance names come from commit messages and flags, not from a validated
/// config, so a backtick in one is plausible; a single-backtick span would close
/// early and garble the rest of the line. CommonMark allows a fence of any
/// length that does not occur inside.
///
/// Padded with one space each side whenever the text holds a backtick or starts
/// or ends with a space. CommonMark strips one leading and one trailing space
/// from a span that has both, so unpadded, `" startup "` renders exactly like
/// `startup` — two benchmark names the flag deliberately keeps apart would
/// read as one in the report. The padding is what gets stripped, leaving the
/// name as written. A span of only spaces is never stripped, so it is left
/// unpadded.
///
/// Control characters are written as escapes (`\n`, `\u{1b}`). The report is
/// read line by line — tak's own perf-pr workflow decides a check's outcome by
/// what a line starts with — and a newline in an `--accept` value would let a
/// name begin a line of its own that reads as a verdict.
pub(crate) fn code(text: &str) -> String {
    let text = escape_control(text);
    let text = text.as_ref();
    let mut longest = 0;
    let mut run = 0;
    for ch in text.chars() {
        run = if ch == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    let edge_space = text.starts_with(' ') || text.ends_with(' ');
    let only_spaces = text.chars().all(|c| c == ' ');
    if (longest > 0 || edge_space) && !only_spaces {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// Plain `text` made safe for one Markdown table cell: an unescaped `|` in a
/// name from `tak.toml` or a note ends the cell early and shifts every column
/// after it.
///
/// Every `\|` is an escaped pipe to the table, whatever precedes it, and the
/// table strips that backslash before the inline parse — which then reads the
/// cell's own backslashes as escapes. So backslashes are doubled first, or a
/// subject's `\|` would render as a bare `|`, and `a\*b` as `a*b`. For a cell
/// whose text sits inside a code span, use [`code_cell`] instead.
pub(crate) fn cell(text: &str) -> String {
    text.replace('\\', "\\\\").replace('|', "\\|")
}

/// [`cell`] for text inside a code span, as [`describe`] writes names:
/// pipes only.
///
/// The table strips a pipe's escaping backslash before the code span is
/// parsed, so `\|` still works there, but a code span shows every other
/// backslash as written. Doubling them, as [`cell`] does, put `win\\arm` in
/// the report for a runner named `win\arm`. One case cannot be written
/// exactly: a backslash directly before a pipe (`a\|b`) is left alone by the
/// table, so it renders as `a\\|b`. The row stays intact either way, checked
/// against comrak's GFM tables.
pub(crate) fn code_cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// The [`Comparison::gate_source`] line: where the gate came from, when the
/// base had no `tak.toml` or the head's would gate differently; `None` when
/// there is nothing to say.
///
/// `path` is the base's `tak.toml` from the repository root, `at` the base
/// commit's short SHA, and `changed` what [`Gates::changes_from`] and the
/// caller named, each already a code span. Every interpolation goes through
/// [`code`]: the path is a directory name, which can hold a backtick that a
/// hand-written span would end early on, or a control character.
pub fn gate_source(path: Option<&str>, at: &str, changed: &[String]) -> Option<String> {
    let later = (!changed.is_empty()).then(|| {
        format!(
            " This revision changes the gate policy ({}), and the change takes effect \
             once it is merged.",
            changed.join(", ")
        )
    });
    match (path, later) {
        (None, later) => Some(format!(
            "No {} at the base, {}, so the gate is tak's defaults plus any flags and \
             environment variables.{}",
            code(crate::config::FILE_NAME),
            code(at),
            later.unwrap_or_default()
        )),
        (Some(path), Some(later)) => Some(format!(
            "The gate comes from {} at the base, {}.{later}",
            code(path),
            code(at)
        )),
        (Some(_), None) => None,
    }
}

/// One line naming trailers that were ignored, so an author whose trailer did
/// nothing can see why rather than assume tak failed to read it.
fn ignored_trailers(c: &Comparison) -> String {
    if c.ignored_trailers.is_empty() {
        return String::new();
    }
    format!(
        "\n`{}` trailers were found but not honoured, because `gate.accept_trailers` is \
         off: {}\n",
        crate::accept::TRAILER,
        c.ignored_trailers
            .iter()
            .map(|(bench, _)| format!("{} ({})", code(bench), c.ignored_trailers.describe(bench)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Acceptances that did not accept anything, each with the reason.
///
/// Reported rather than dropped. A stale acceptance is harmless today and a
/// surprise the day that benchmark does regress; a misspelt one means the
/// regression it was written for still fails, and the author needs to see why.
/// A report-only benchmark is its own case: it can never fail, so accepting it
/// does nothing, and saying "did not rise" would be wrong when it did. None of
/// these fail the gate by themselves.
fn unused_acceptances(c: &Comparison, gates: &Gates) -> String {
    let mut quiet = Vec::new();
    let mut report_only = Vec::new();
    let mut unknown = Vec::new();
    for (bench, _) in c.accepted.iter() {
        let entry = format!("{} ({})", code(bench), c.accepted.describe(bench));
        let counted: Vec<&Change> = c
            .changes
            .iter()
            .filter(|ch| ch.bench == bench && ch.metric == GATED_METRIC)
            .collect();
        if !c.changes.iter().any(|ch| ch.bench == bench) {
            unknown.push(entry);
        } else if counted.iter().any(|ch| {
            let gate = gates.of(ch);
            gate.enabled && ch.exceeds(&gate)
        }) {
            // Accepted something.
        } else if !counted.is_empty() && counted.iter().all(|ch| !gates.of(ch).enabled) {
            report_only.push(entry);
        } else {
            quiet.push(entry);
        }
    }
    let mut out = String::new();
    if !quiet.is_empty() {
        out.push_str(&format!(
            "\nAccepted, but not above its gate, so nothing was accepted: {}\n",
            quiet.join(", ")
        ));
    }
    if !report_only.is_empty() {
        out.push_str(&format!(
            "\nAccepted, but report-only, so it can never fail and nothing was accepted: {}\n",
            report_only.join(", ")
        ));
    }
    if !unknown.is_empty() {
        out.push_str(&format!(
            "\nAccepted, but no benchmark by that name was compared on both sides: {}\n",
            unknown.join(", ")
        ));
    }
    out
}

/// An allocation count's change, for display.
///
/// A zero base has no percentage, and [`signed_pct`] calls that `new`, which
/// is right for an instruction count: a benchmark that retired nothing was
/// not really measured. A zero allocation count is a real measurement — a
/// command that allocated nothing — so `0 → 0` is no change and `0 → N` is
/// the rise itself. `new` stays for a series with nothing on the base side,
/// which never reaches this table.
fn count_delta(ch: &Change) -> String {
    match ch.pct() {
        Some(p) => signed_pct(Some(p)),
        None if ch.head == ch.base => signed_pct(Some(0.0)),
        None => format!("+{} (from 0)", thousands(ch.head - ch.base)),
    }
}

/// `text` with every control character written as its escape (`\n`,
/// `\u{1b}`), borrowed unchanged in the usual case of there being none.
///
/// A report is read line by line, and tak's own workflows decide a check by
/// what a line starts with. A name holding a newline could begin a line of its
/// own that reads as a verdict, so no name reaches the report with one.
pub fn escape_control(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return std::borrow::Cow::Borrowed(text);
    }
    text.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect::<String>()
        .into()
}

/// How a series is named in prose: bench, plus the tool when it is not the
/// project itself, plus the runner.
///
/// Dropping the tool made two series that differ only by tool render
/// identically, so a report could say the same benchmark both started and
/// stopped gating and mean two different programs.
///
/// Control characters are escaped: `tak detect` names accepted steps through
/// this, and a benchmark name comes from the `tak.toml` under test.
pub fn describe(key: &Key) -> String {
    let (bench, tool, runner) = key;
    let (bench, tool, runner) = (
        escape_control(bench),
        escape_control(tool),
        escape_control(runner),
    );
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
    use crate::accept::Source;

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

    /// Only effective differences are named, and every name is escaped: a
    /// declared benchmark at the global gate is no change, and a newline in a
    /// measured series' name cannot start a line of the report.
    #[test]
    fn a_gate_source_path_with_a_backtick_stays_one_code_span() {
        let changed = [code("[gate]")];
        let line = gate_source(Some("we`ird/tak.toml"), "a1b2c3d4e5f6", &changed).unwrap();
        assert_eq!(
            line,
            "The gate comes from `` we`ird/tak.toml `` at the base, `a1b2c3d4e5f6`. \
             This revision changes the gate policy (`[gate]`), and the change takes \
             effect once it is merged."
        );
        let line = gate_source(Some("a\nb/tak.toml"), "a1b2c3d4e5f6", &changed).unwrap();
        assert!(!line.contains('\n'), "{line}");
        assert_eq!(gate_source(Some("tak.toml"), "a1b2c3d4e5f6", &[]), None);
        assert_eq!(
            gate_source(None, "a1b2c3d4e5f6", &[]).unwrap(),
            "No `tak.toml` at the base, `a1b2c3d4e5f6`, so the gate is tak's defaults \
             plus any flags and environment variables."
        );
    }

    #[test]
    fn gate_changes_are_effective_and_escaped() {
        let global = gate(1.0, 0, true);
        let base = Gates::uniform(global);
        let mut head = Gates::uniform(global);
        head.set_bench("new", global);
        head.set_series("new", SELF_TOOL, global);
        assert!(head.changes_from(&base, &BTreeSet::new()).is_empty());

        let evil = "a\n**0 benchmark(s) above the 1% gate:**".to_string();
        head.set_series(&evil, SELF_TOOL, gate(1.0, 0, false));
        let measured = BTreeSet::from([(evil, SELF_TOOL.to_string())]);
        let changes = head.changes_from(&base, &measured);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(!changes[0].contains('\n'), "{changes:?}");
        assert!(changes[0].starts_with('`'), "{changes:?}");
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

    fn with_allocs(mut r: Record, blocks: f64, bytes: f64, peak: f64) -> Record {
        r.metrics.insert("alloc_blocks".into(), blocks);
        r.metrics.insert("alloc_bytes".into(), bytes);
        r.metrics.insert("alloc_peak_bytes".into(), peak);
        r
    }

    /// A project that never opted in gets the report it always did: no
    /// allocation table, no empty columns.
    #[test]
    fn the_allocation_table_appears_only_when_both_sides_have_allocations() {
        let plain = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 1_000_000.0, 10.0)],
        );
        let md = markdown(&plain, &Trend::new(), &g(1.0), false);
        assert!(!md.contains("Heap allocations"), "{md}");
        assert!(md.contains("| benchmark | instructions | Δ | wall (min) | Δ |"));

        // The first run after opting in has nothing to compare against.
        let first = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[with_allocs(
                rec("a", "gha", 1_000_000.0, 10.0),
                3.0,
                4140.0,
                4140.0,
            )],
        );
        assert!(!markdown(&first, &Trend::new(), &g(1.0), false).contains("Heap allocations"));
    }

    /// Allocations are reported beside the gate, never part of it, however
    /// far they move.
    #[test]
    fn allocations_are_reported_and_never_gate() {
        let c = compare(
            &[with_allocs(
                rec("a", "gha", 1_000_000.0, 10.0),
                31.0,
                7_119.0,
                6_847.0,
            )],
            &[with_allocs(
                rec("a", "gha", 1_000_000.0, 10.0),
                62.0,
                14_238.0,
                6_847.0,
            )],
        );
        assert!(c.regressions(&g(0.001)).is_empty());
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(md.contains("reported, not gated"), "{md}");
        assert!(
            md.contains(
                "| a | 31 → 62 | +100.00% | 7,119 → 14,238 | +100.00% | 6,847 → 6,847 | +0.00% |"
            ),
            "{md}"
        );
        assert!(md.contains("No instruction-count regression"), "{md}");
    }

    /// The same benchmark on two runner classes is two rows, and each has
    /// to say which runner it is, in both tables. One runner needs no label.
    #[test]
    fn rows_name_their_runner_only_when_another_shares_the_series() {
        let on = |runner| with_allocs(rec("a", runner, 1_000_000.0, 10.0), 3.0, 30.0, 20.0);
        let both = [on("linux-x64"), on("linux-arm64")];
        let md = markdown(&compare(&both, &both), &Trend::new(), &g(1.0), false);
        for runner in ["linux-x64", "linux-arm64"] {
            let rows = md
                .lines()
                .filter(|l| l.starts_with(&format!("| a on {runner} |")))
                .count();
            assert_eq!(
                rows, 2,
                "instructions and allocations rows for {runner}: {md}"
            );
        }

        let one = [on("linux-x64")];
        let md = markdown(&compare(&one, &one), &Trend::new(), &g(1.0), false);
        assert!(
            md.contains("| a |") && !md.contains(" on linux-x64 |"),
            "{md}"
        );
    }

    /// Acceptances for each comma-separated name. A test convenience only:
    /// neither real source splits on commas.
    fn accepting(names: &str, source: Source) -> Acceptances {
        let mut a = Acceptances::default();
        for name in names.split(',') {
            a.add_name(name.trim(), source.clone()).unwrap();
        }
        a
    }

    /// A name cannot start a line of its own, so it can never read as a
    /// verdict to something that matches on line starts.
    #[test]
    fn a_control_character_in_a_name_is_escaped() {
        assert_eq!(code("a\nb"), "`a\\nb`");
        assert_eq!(
            describe(&("a\nb".into(), "self".into(), "r\r".into())),
            "`a\\nb` on `r\\r`"
        );
        // The usual case is untouched.
        assert_eq!(describe(&key("a")), "`a` on `gha`");
        let c = compare(&[], &[]).with_accepted(accepting(
            "x\n**1 benchmark(s) above the 1% gate:** `x`",
            Source::Flag,
        ));
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(
            md.lines().all(|l| !l.starts_with("**1 benchmark(s)")),
            "{md}"
        );
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// The point of the feature: one benchmark's deliberate regression passes
    /// the gate while another's still fails it.
    #[test]
    fn an_acceptance_covers_only_the_benchmark_it_names() {
        let c = compare(
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("b", "gha", 1_000_000.0, 10.0),
            ],
            &[
                rec("a", "gha", 1_100_000.0, 10.0),
                rec("b", "gha", 1_100_000.0, 10.0),
            ],
        )
        .with_accepted(accepting("a", Source::Trailer(SHA.into())));
        assert_eq!(
            c.regressions(&g(1.0)).len(),
            2,
            "both still count as regressed"
        );
        let failures = c.failures(&g(1.0));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].bench, "b");
    }

    /// Accepted is not the same as passing, and the report must not let the
    /// two be confused: the row is marked, and the verdict names the source.
    #[test]
    fn an_accepted_regression_stays_visible_with_its_source() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 1_100_000.0, 10.0)],
        )
        .with_accepted(accepting("a", Source::Trailer(SHA.into())));
        assert!(c.failures(&g(1.0)).is_empty());
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(md.contains("**+10.00%** (accepted)"), "{md}");
        assert!(
            md.contains("**1 accepted regression(s) above the 1% gate, not failing it:** `a` on `gha` +10.00% (`Tak-Accept` in `0123456789ab`)"),
            "{md}"
        );
        assert!(!md.contains("No instruction-count regression"), "{md}");
        assert!(!md.contains("⚠️"), "{md}");
    }

    #[test]
    fn failures_and_acceptances_are_reported_separately() {
        let c = compare(
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("b", "gha", 1_000_000.0, 10.0),
            ],
            &[
                rec("a", "gha", 1_100_000.0, 10.0),
                rec("b", "gha", 1_200_000.0, 10.0),
            ],
        )
        .with_accepted(accepting("a", Source::Flag));
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(
            md.contains("**1 benchmark(s) above the 1% gate:** `b` +20.00%"),
            "{md}"
        );
        assert!(md.contains("`a` on `gha` +10.00% (`--accept`)"), "{md}");
    }

    /// One name covers every tool and runner the benchmark was measured with.
    #[test]
    fn an_acceptance_covers_every_series_of_its_benchmark() {
        let mut other = rec("a", "gha", 1_000_000.0, 10.0);
        other.tool = "other".into();
        let mut other_head = other.clone();
        other_head.metrics.insert(GATED_METRIC.into(), 2_000_000.0);
        let c = compare(
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("a", "arm", 1_000_000.0, 10.0),
                other,
            ],
            &[
                rec("a", "gha", 2_000_000.0, 10.0),
                rec("a", "arm", 2_000_000.0, 10.0),
                other_head,
            ],
        )
        .with_accepted(accepting("a", Source::Flag));
        assert_eq!(c.regressions(&g(1.0)).len(), 3);
        assert!(c.failures(&g(1.0)).is_empty());
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(md.contains("`a (other)` on `gha` +100.00%"), "{md}");
    }

    /// An acceptance that accepted nothing is reported: stale, it would
    /// silently cover the next real regression; misspelt, it explains why the
    /// gate still failed. Neither fails the gate on its own.
    #[test]
    fn an_acceptance_with_nothing_to_accept_is_reported() {
        let c = compare(
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("b", "gha", 1_000_000.0, 10.0),
            ],
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("b", "gha", 1_100_000.0, 10.0),
            ],
        )
        .with_accepted(accepting("a,bb", Source::Flag));
        assert_eq!(c.failures(&g(1.0)).len(), 1, "a typo must not accept `b`");
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(
            md.contains(
                "Accepted, but not above its gate, so nothing was accepted: `a` (`--accept`)"
            ),
            "{md}"
        );
        assert!(
            md.contains("no benchmark by that name was compared on both sides: `bb` (`--accept`)"),
            "{md}"
        );
    }

    /// A `|` in a name is escaped, so it cannot add a column.
    #[test]
    fn a_pipe_in_a_row_name_is_escaped() {
        let on = |runner| rec("a|b", runner, 1_000_000.0, 10.0);
        let both = [on("ci|x64"), on("ci|arm64")];
        let md = markdown(&compare(&both, &both), &Trend::new(), &g(1.0), false);
        assert!(md.contains("| a\\|b on ci\\|x64 |"), "{md}");
        let row = md.lines().find(|l| l.contains("ci\\|arm64")).unwrap();
        let cells = row.replace("\\|", "").matches('|').count();
        assert_eq!(cells, 6, "a benchmark cell and four numbers: {row}");
    }

    /// The allocation table's labels go through the same escaping as every
    /// other report site: a control character in a bench, tool or runner
    /// name from an old note cannot start a line of its own, and a
    /// backslash is kept as written.
    #[test]
    fn allocation_labels_are_escaped_like_the_rest_of_the_report() {
        let on = |runner| {
            let mut r = with_allocs(rec("a\nb", runner, 1.0, 1.0), 3.0, 30.0, 20.0);
            r.tool = "t\\x".into();
            r
        };
        let both = [on("ci\rx64"), on("ci-arm64")];
        let md = markdown(&compare(&both, &both), &Trend::new(), &g(1.0), false);
        let allocs = md
            .lines()
            .skip_while(|l| !l.starts_with("Heap allocations"))
            .collect::<Vec<_>>();
        assert!(
            allocs
                .iter()
                .any(|l| l.starts_with(r"| a\\nb (t\\x) on ci\\rx64 |")),
            "{md}"
        );
        assert!(!md.contains('\r'), "{md:?}");
    }

    /// A zero allocation count is a measurement, not a missing base: no
    /// change reads as such, and a rise from zero gives the rise.
    #[test]
    fn a_zero_allocation_base_is_a_measurement_not_new() {
        let c = compare(
            &[with_allocs(
                rec("a", "gha", 1_000_000.0, 10.0),
                0.0,
                0.0,
                0.0,
            )],
            &[with_allocs(
                rec("a", "gha", 1_000_000.0, 10.0),
                0.0,
                4_140.0,
                0.0,
            )],
        );
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        let row = md
            .lines()
            .skip_while(|l| !l.starts_with("Heap allocations"))
            .find(|l| l.starts_with("| a |"))
            .unwrap();
        assert_eq!(
            row,
            "| a | 0 → 0 | +0.00% | 0 → 4,140 | +4,140 (from 0) | 0 → 0 | +0.00% |"
        );
        assert!(!row.contains("new"), "{row}");
        assert!(c.regressions(&g(0.001)).is_empty(), "still never gates");
    }

    /// With nothing compared there is no table, but an acceptance still has
    /// to be accounted for — this is the state a runner migration produces.
    #[test]
    fn an_acceptance_is_reported_when_nothing_was_compared() {
        let c =
            compare(&[], &[rec("a", "gha", 1.0, 1.0)]).with_accepted(accepting("a", Source::Flag));
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(md.contains("nothing was gated"), "{md}");
        assert!(
            md.contains("no benchmark by that name was compared"),
            "{md}"
        );
    }

    /// Acceptance only ever relaxes the gate for a regression. An improvement
    /// under an acceptance is still just an improvement.
    #[test]
    fn an_acceptance_never_invents_a_regression() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 500_000.0, 10.0)],
        )
        .with_accepted(accepting("a", Source::Flag));
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(
            md.contains("No instruction-count regression above 1%"),
            "{md}"
        );
        assert!(!md.contains("(accepted)"), "{md}");
    }

    /// Ignored trailers are reported and change nothing else: the regression
    /// still fails, and nothing is marked accepted.
    #[test]
    fn an_ignored_trailer_is_named_and_accepts_nothing() {
        let c = compare(
            &[rec("a", "gha", 1_000_000.0, 10.0)],
            &[rec("a", "gha", 1_100_000.0, 10.0)],
        )
        .with_ignored_trailers(accepting("a", Source::Trailer(SHA.into())));
        assert_eq!(c.failures(&g(1.0)).len(), 1);
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(
            md.contains("trailers were found but not honoured, because `gate.accept_trailers` is off: `a` (`Tak-Accept` in `0123456789ab`)"),
            "{md}"
        );
        assert!(!md.contains("(accepted)"), "{md}");
    }

    /// A backtick in a name must not close the code span it is shown in.
    #[test]
    fn a_name_with_backticks_keeps_its_code_span() {
        assert_eq!(code("startup"), "`startup`");
        assert_eq!(code("a`b"), "`` a`b ``");
        assert_eq!(code("``x"), "``` ``x ```");
        let c = compare(&[], &[]).with_accepted(accepting("we`ird", Source::Flag));
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(md.contains("`` we`ird `` (`--accept`)"), "{md}");
    }

    /// CommonMark strips one space from each side of a span that has both, so
    /// a name with edge spaces is padded to survive that and render as itself.
    /// Unpadded, `" startup "` and `startup` rendered identically on GitHub.
    #[test]
    fn a_name_with_edge_spaces_keeps_them() {
        assert_eq!(code(" startup "), "`  startup  `");
        assert_eq!(code(" startup"), "`  startup `");
        assert_eq!(code("startup "), "` startup  `");
        assert_ne!(code(" startup "), code("startup"));
        // A span of only spaces is not stripped, so it needs no padding.
        assert_eq!(code("  "), "`  `");
    }

    #[test]
    fn a_cell_escapes_backslashes_before_pipes() {
        assert_eq!(cell("startup"), "startup");
        assert_eq!(cell("a|b"), r"a\|b");
        // Doubled so the inline parse gives them back as written: comrak
        // renders `a\\\|b` as `a\|b` and `win\\arm` as `win\arm`.
        assert_eq!(cell(r"a\|b"), r"a\\\|b");
        assert_eq!(cell(r"win\arm"), r"win\\arm");
    }

    /// Inside a code span a backslash is shown as written, so only pipes are
    /// escaped: comrak renders `` `win\arm` `` as `win\arm` and `` `a\|b` ``
    /// as `a|b`, each in one cell.
    #[test]
    fn a_code_cell_escapes_only_pipes() {
        assert_eq!(code_cell("`win\\arm`"), "`win\\arm`");
        assert_eq!(code_cell("`a|b`"), r"`a\|b`");
        // The one inexact case: this renders as `a\\|b`, still in one cell.
        assert_eq!(code_cell(r"`a\|b`"), r"`a\\|b`");
    }

    /// A name with a pipe in it stays in its own cell, so every row keeps the
    /// header's column count — the baseline report renders through here too.
    #[test]
    fn a_pipe_in_a_name_does_not_split_the_row() {
        let mut base = rec("a|b", "gha", 100.0, 1.0);
        let mut head = rec("a|b", "gha", 101.0, 1.0);
        base.tool = "x|y".into();
        head.tool = "x|y".into();
        let md = markdown(&compare(&[base], &[head]), &Trend::new(), &g(1.0), false);
        assert!(md.contains(r"| a\|b (x\|y) |"), "{md}");
        let columns = |l: &str| l.replace(r"\|", "").matches('|').count();
        let rows: Vec<&str> = md.lines().filter(|l| l.starts_with('|')).collect();
        assert_eq!(rows.len(), 3, "{md}");
        assert!(rows.iter().all(|r| columns(r) == columns(rows[0])), "{md}");
    }

    /// Two runner classes of one accepted benchmark are two entries, and the
    /// verdict has to say which is which.
    #[test]
    fn an_accepted_entry_names_its_runner() {
        let c = compare(
            &[
                rec("a", "gha", 1_000_000.0, 10.0),
                rec("a", "arm", 1_000_000.0, 10.0),
            ],
            &[
                rec("a", "gha", 1_100_000.0, 10.0),
                rec("a", "arm", 1_200_000.0, 10.0),
            ],
        )
        .with_accepted(accepting("a", Source::Flag));
        let md = markdown(&c, &Trend::new(), &g(1.0), false);
        assert!(md.contains("`a` on `arm` +20.00% (`--accept`)"), "{md}");
        assert!(md.contains("`a` on `gha` +10.00% (`--accept`)"), "{md}");
    }

    /// Each series is judged against its own effective gate before acceptance
    /// is consulted. `startup` at 5% with a floor did not regress, so accepting
    /// it waived nothing; `install` at the global 1% did, and was waived.
    #[test]
    fn an_acceptance_is_judged_against_the_series_own_gate() {
        let mut gates = g(1.0);
        gates.set_series("startup", SELF_TOOL, gate(5.0, 20_000, true));
        let c = startup_and_install().with_accepted(accepting("startup, install", Source::Flag));
        assert!(c.failures(&gates).is_empty());
        assert_eq!(benches(&c.accepted_regressions(&gates)), ["install"]);
        let md = markdown(&c, &Trend::new(), &gates, false);
        assert!(
            md.contains(
                "**1 accepted regression(s) above their gate, not failing it:** \
                 `install` on `gha` +2.00% (gate 1%) (`--accept`)\n"
            ),
            "{md}"
        );
        assert!(
            md.contains("Accepted, but not above its gate, so nothing was accepted: `startup`"),
            "{md}"
        );
        assert!(!md.contains("No gated benchmark rose"), "{md}");
    }

    /// The floor decides first. A rise under `min_delta` is not a regression,
    /// so an acceptance naming it accepted nothing and the report says so.
    #[test]
    fn an_acceptance_does_not_count_a_rise_under_the_floor() {
        let gates = Gates::uniform(Gate::new(1.0, 10_000).unwrap());
        let c = startup_and_install().with_accepted(accepting("startup, install", Source::Flag));
        assert!(c.failures(&gates).is_empty());
        assert_eq!(benches(&c.accepted_regressions(&gates)), ["install"]);
        let md = markdown(&c, &Trend::new(), &gates, false);
        assert!(
            md.contains(
                "**1 accepted regression(s) above the 1% gate, not failing it:** \
                 `install` on `gha` +2.00% (`--accept`)\n"
            ),
            "{md}"
        );
        assert!(
            md.contains("not above its gate, so nothing was accepted: `startup`"),
            "{md}"
        );
    }

    /// A report-only series can never fail, so it never needs accepting. An
    /// acceptance naming one is listed with that reason, and the row keeps its
    /// report-only marking rather than claiming an acceptance happened.
    #[test]
    fn accepting_a_report_only_benchmark_accepts_nothing() {
        let mut gates = g(1.0);
        gates.set_series("startup", SELF_TOOL, gate(1.0, 0, false));
        let c = startup_and_install().with_accepted(accepting("startup", Source::Flag));
        assert_eq!(benches(&c.failures(&gates)), ["install"]);
        assert!(c.accepted_regressions(&gates).is_empty());
        let md = markdown(&c, &Trend::new(), &gates, false);
        assert!(md.contains("**+2.00%** (not gated)"), "{md}");
        assert!(!md.contains("(accepted)"), "{md}");
        assert!(
            md.contains(
                "Accepted, but report-only, so it can never fail and nothing was \
                 accepted: `startup` (`--accept`)"
            ),
            "{md}"
        );
        assert!(
            md.contains("**1 report-only benchmark(s) above their gate, not failing:**"),
            "{md}"
        );
    }

    /// With nothing accepted, a per-benchmark report is exactly what it was
    /// without acceptance support: every line this adds is conditional.
    #[test]
    fn an_empty_acceptance_leaves_a_per_benchmark_report_unchanged() {
        let mut gates = g(1.0);
        gates.set_series("startup", SELF_TOOL, gate(5.0, 20_000, true));
        let plain = markdown(&startup_and_install(), &Trend::new(), &gates, false);
        let c = startup_and_install()
            .with_accepted(Acceptances::default())
            .with_ignored_trailers(Acceptances::default());
        assert_eq!(markdown(&c, &Trend::new(), &gates, false), plain);
        assert!(!plain.contains("accepted"), "{plain}");
    }

    #[test]
    fn thousands_separates() {
        assert_eq!(thousands(1234567.0), "1,234,567");
        assert_eq!(thousands(999.0), "999");
        assert_eq!(thousands(1000.0), "1,000");
    }
}
