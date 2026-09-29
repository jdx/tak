//! Find instruction-count steps that have already landed on a branch.
//!
//! `tak compare` gates a pull request before it merges. Nothing looked at what
//! merged anyway: a gate that was not required, an override, a direct push, or
//! several changes that each stayed under the threshold. This walks a branch's
//! recorded first-parent history and reports where each series stepped.
//!
//! A step detector, deliberately, not a change-point engine. E-divisive means
//! (Hunter) and its relatives exist to find shifts in noisy data. Instruction
//! counts reproduce to ~0.02%, so a move between consecutive recorded points
//! larger than a 1% gate *is* the change point, and a statistical model would
//! only add ways to be wrong. Wall clock is noisy enough to need one, and it is
//! exactly the metric that must never fail a build — so it is shown beside a
//! step for context and never examined for one.
//!
//! Everything below [`gather`] is a pure function over `(sha, records)`
//! sequences, so the rules are tested without a repository.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

use crate::compare::{
    CREDIT, Change, GATED_METRIC, Gate, Gates, Key, Trend, WALL_METRIC, describe, signed_pct,
    sparkline, thousands,
};
use crate::notes;
use crate::record::Record;

/// How many first-parent commits a walk may cover while filling its window.
///
/// A bound rather than the whole history because `rev-list` output is held in
/// memory and a monorepo trunk can run to hundreds of thousands of commits. Ten
/// thousand is far more than any window needs unless recordings are extremely
/// sparse, and still lists in tens of milliseconds. Reaching it before the
/// window fills is reported as a [`Cutoff`], never passed over silently.
pub const SCAN_LIMIT: usize = 10_000;

/// Why a walk stopped before its window filled, when the reason is the walk's
/// and not the project's.
///
/// Either way a recording further back exists, or may exist, and was never
/// compared. Without saying so, a series whose previous point lay just past
/// the boundary reads as brand new, and a step onto the head goes unchecked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cutoff {
    /// The checkout is a shallow clone and the walk reached its boundary.
    Shallow,
    /// The walk covered [`SCAN_LIMIT`] first-parent commits.
    ScanLimit,
}

/// Decide whether the walk was cut short, and by what.
///
/// `truncated` says history continued past [`SCAN_LIMIT`] — see [`gather`]
/// for how that is known rather than guessed. Otherwise the walk reached the
/// end of the history this checkout has: the root, or a shallow clone's
/// boundary, which only `shallow` can tell apart. A window that filled was cut
/// short by neither, however deep the history.
pub fn cutoff(
    recorded: usize,
    window: usize,
    truncated: bool,
    shallow: impl FnOnce() -> Result<bool>,
) -> Result<Option<Cutoff>> {
    if recorded >= window {
        return Ok(None);
    }
    if truncated {
        return Ok(Some(Cutoff::ScanLimit));
    }
    Ok(shallow()?.then_some(Cutoff::Shallow))
}

/// One series' reduced value at one commit.
#[derive(Debug, Clone, Copy)]
struct Point {
    /// Index into the walked commits, oldest first.
    at: usize,
    instructions: f64,
    wall: Option<f64>,
}

/// A move between two consecutive recorded points of one series.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub key: Key,
    /// The earlier recorded commit, which the step is measured from.
    pub from: String,
    /// The later recorded commit, where the new value was first seen.
    pub to: String,
    /// First-parent commits in `from..to`. One means the step belongs to `to`
    /// alone; more means it happened somewhere in the range and the report must
    /// say so, because naming `to` would blame a commit that may be innocent.
    pub commits: usize,
    pub instructions: Change,
    /// For context only. [`Change::exceeds`] refuses to gate on it.
    pub wall: Option<Change>,
}

impl Step {
    /// The gate this step's series is held to — the same lookup `tak compare`
    /// uses, so a benchmark's `tak.toml` gate means the same thing before a
    /// merge and after it.
    pub fn gate(&self, gates: &Gates) -> Gate {
        gates.get(&self.key.0, &self.key.1)
    }

    /// Beyond its series' threshold and floor, whether or not that gate is
    /// enabled. A report-only series still crosses its threshold, and the
    /// report says so.
    pub fn exceeds(&self, gates: &Gates) -> bool {
        self.instructions.exceeds(&self.gate(gates))
    }

    /// Beyond an enabled gate: a step that is allowed to fail the command.
    pub fn fails(&self, gates: &Gates) -> bool {
        self.gate(gates).enabled && self.exceeds(gates)
    }
}

/// A rise that no single step explains.
#[derive(Debug, Clone, PartialEq)]
pub struct Drift {
    pub key: Key,
    /// Where the run of sub-threshold steps began.
    pub since: String,
    /// Recorded points in that run, both ends included.
    pub points: usize,
    pub change: Change,
}

#[derive(Debug, PartialEq)]
pub struct Detection {
    pub head: String,
    /// Every series' gate, from `tak.toml` and `[gate]`. Policy of whoever runs
    /// the check, as for `tak compare`: never read from the notes.
    pub gates: Gates,
    /// First-parent commits in the walk, recorded or not.
    pub walked: usize,
    /// How many of them carry any measurement.
    pub recorded: usize,
    /// For every series measured at the head with an earlier point in the
    /// window: its step onto the head. The only steps that can fail.
    pub latest: Vec<Step>,
    /// Steps above the gate that end before the head. Report-only.
    pub earlier: Vec<Step>,
    /// Report-only.
    pub drift: Vec<Drift>,
    /// Measured at the head with nothing earlier in the window to compare to.
    pub new_at_head: Vec<Key>,
    /// Measured earlier in the window and not at the head.
    pub missing_at_head: Vec<Key>,
    pub trend: Trend,
    /// Set when the walk stopped before the window filled for a reason of its
    /// own, so a short history here may be the walk's rather than the project's.
    pub cutoff: Option<Cutoff>,
    /// Whether an empty comparison was accepted (`--allow-empty`) rather than
    /// failed. Carried here so the report and the exit status cannot disagree.
    pub allow_empty: bool,
    /// Report-only (`--no-gate`): nothing fails, an empty comparison included.
    /// The same promise `tak compare --no-gate` makes, so a workflow that must
    /// always exit successfully needs one flag, not two.
    pub no_gate: bool,
}

impl Detection {
    /// A walk that found nothing, held to `gates`.
    pub fn new(head: String, gates: Gates) -> Self {
        Self {
            head,
            gates,
            walked: 0,
            recorded: 0,
            latest: vec![],
            earlier: vec![],
            drift: vec![],
            new_at_head: vec![],
            missing_at_head: vec![],
            trend: Trend::new(),
            cutoff: None,
            allow_empty: false,
            no_gate: false,
        }
    }

    /// Steps onto the head beyond an enabled gate: what makes the command fail.
    ///
    /// Only the head's. A main-branch workflow runs this once per push, so
    /// failing on an older step would fail every run after the one that caused
    /// it, and a job that is always red is a job nobody reads.
    pub fn failures(&self) -> Vec<&Step> {
        self.latest
            .iter()
            .filter(|s| s.fails(&self.gates))
            .collect()
    }

    /// Steps onto the head beyond a report-only gate: flagged, never failed on.
    pub fn reported(&self) -> Vec<&Step> {
        self.latest
            .iter()
            .filter(|s| !s.gate(&self.gates).enabled && s.exceeds(&self.gates))
            .collect()
    }

    /// Whether every series in the report is held to the global gate.
    ///
    /// When it is, the report reads exactly as it did before per-benchmark
    /// gates existed — one `N%` in the prose, no gate column — for the same
    /// reason `tak compare` keeps its wording: a project that never wrote a
    /// per-benchmark gate should not have its output change under it.
    pub fn uniform(&self) -> bool {
        let keys = self
            .latest
            .iter()
            .chain(&self.earlier)
            .map(|s| &s.key)
            .chain(self.drift.iter().map(|d| &d.key));
        keys.into_iter()
            .all(|k| self.gates.get(&k.0, &k.1) == self.gates.global)
    }

    /// No series at the head had anything to be compared against.
    pub fn nothing_compared(&self) -> bool {
        self.latest.is_empty()
    }

    /// Whether comparing nothing fails the command.
    ///
    /// By default it does. A check that passes when it examined nothing is
    /// indistinguishable, from the outside, from one that examined everything
    /// and found it clean — and the ways to get here by accident (a shallow
    /// checkout, a recording step that stopped producing instruction counts, a
    /// runner class that drifted) are exactly the ones nobody reads a green
    /// job's summary to find. The legitimate cases, a first recording or a new
    /// runner class, are rare and known in advance, so they are the ones asked
    /// to opt out.
    pub fn empty_fails(&self) -> bool {
        self.nothing_compared() && !self.allow_empty && !self.no_gate
    }
}

/// Choose the commits to walk: from the head back to the `window`-th recorded
/// commit, returned oldest first.
///
/// The window counts *recorded* commits, not commits. A push of several commits
/// records only its tip, so a window of twenty commits could hold two points,
/// and whether it held enough would depend on how people happened to push.
///
/// The head is always included, recorded or not — "the head has nothing
/// recorded" is itself something the report has to say. Unrecorded commits
/// older than the oldest recorded one are dropped: they belong to no range.
pub fn select(newest_first: &[String], annotated: &BTreeSet<String>, window: usize) -> Vec<String> {
    let mut seen = 0;
    let mut end = newest_first.len().min(1);
    for (i, sha) in newest_first.iter().enumerate() {
        if annotated.contains(sha) {
            seen += 1;
            end = i + 1;
            if seen >= window {
                break;
            }
        }
    }
    newest_first[..end].iter().rev().cloned().collect()
}

/// Commits oldest first, each with the records attached to it — empty where
/// nothing was recorded, which is what makes a range visible.
pub type Walk = Vec<(String, Vec<Record>)>;

/// Walk `head`'s first-parent history and read the notes along it.
///
/// Returns the walk oldest first, plus what cut it short, if anything did.
pub fn gather(head: &str, window: usize) -> Result<(Walk, Option<Cutoff>)> {
    // One more than the limit, then drop it. Returning exactly SCAN_LIMIT is
    // ambiguous — a history of exactly that length returns it too — and
    // reading it as a cut-off sent people looking for recordings that did not
    // exist. An extra commit is proof that history goes on.
    let mut commits = notes::rev_list(head, SCAN_LIMIT + 1)?;
    let truncated = commits.len() > SCAN_LIMIT;
    commits.truncate(SCAN_LIMIT);
    let annotated = notes::annotated()?;
    let chosen = select(&commits, &annotated, window);
    let mut walked = Vec::with_capacity(chosen.len());
    let mut recorded = 0;
    for sha in chosen {
        let records = if annotated.contains(&sha) {
            recorded += 1;
            notes::read(None, &sha)?
        } else {
            vec![]
        };
        walked.push((sha, records));
    }
    // The oldest commit rev-list reached, not the oldest one kept: the walk
    // ended there, so that is where a shallow boundary would have stopped it.
    let cut = cutoff(recorded, window, truncated, || {
        Ok(commits
            .last()
            .is_some_and(|oldest| notes::is_shallow_boundary(oldest)))
    })?;
    Ok((walked, cut))
}

/// One value per instruction-counted series at one commit, minimum across its
/// records — the same reducer `compare` uses, for the same reason: the extra
/// work a machine sometimes does is one-sided, and a re-run must not put a
/// spike in a number meant to be deterministic.
///
/// Series with no instruction count are left out entirely. They cannot gate,
/// and a wall-only series examined for steps would report scheduler noise.
fn fold(records: &[Record]) -> BTreeMap<Key, (f64, Option<f64>)> {
    let mut ins: BTreeMap<Key, f64> = BTreeMap::new();
    let mut wall: BTreeMap<Key, f64> = BTreeMap::new();
    for r in records {
        let key: Key = (r.bench.clone(), r.tool.clone(), r.runner.clone());
        if let Some(&v) = r.metrics.get(GATED_METRIC) {
            ins.entry(key.clone())
                .and_modify(|e| *e = e.min(v))
                .or_insert(v);
        }
        if let Some(&v) = r.metrics.get(WALL_METRIC) {
            wall.entry(key).and_modify(|e| *e = e.min(v)).or_insert(v);
        }
    }
    ins.into_iter()
        .map(|(k, v)| {
            let w = wall.get(&k).copied();
            (k, (v, w))
        })
        .collect()
}

fn step(key: &Key, walked: &[(String, Vec<Record>)], a: Point, b: Point) -> Step {
    let change = |metric: &str, base: f64, head: f64| Change {
        bench: key.0.clone(),
        tool: key.1.clone(),
        runner: key.2.clone(),
        metric: metric.to_string(),
        base,
        head,
    };
    Step {
        key: key.clone(),
        from: walked[a.at].0.clone(),
        to: walked[b.at].0.clone(),
        commits: b.at - a.at,
        instructions: change(GATED_METRIC, a.instructions, b.instructions),
        wall: match (a.wall, b.wall) {
            (Some(x), Some(y)) => Some(change(WALL_METRIC, x, y)),
            _ => None,
        },
    }
}

/// Find the steps in a walk. `walked` is oldest first and ends at the head.
///
/// Each series is keyed on (bench, tool, runner) and only ever compared with
/// itself. Runner is in the key because it has to be: absolute counts shift
/// between machine classes by more than a real regression does, so a runner
/// change must start a new series rather than read as a step.
///
/// Every threshold is the series' own gate: a report-only benchmark is examined
/// like any other and can never fail, and a floor (`min_delta`) applies to
/// steps and drift alike.
pub fn analyze(walked: &[(String, Vec<Record>)], gates: &Gates) -> Detection {
    let Some((head, _)) = walked.last() else {
        return Detection::new(String::new(), gates.clone());
    };
    let head_at = walked.len() - 1;

    let mut series: BTreeMap<Key, Vec<Point>> = BTreeMap::new();
    for (at, (_, records)) in walked.iter().enumerate() {
        for (key, (instructions, wall)) in fold(records) {
            series.entry(key).or_default().push(Point {
                at,
                instructions,
                wall,
            });
        }
    }

    let mut d = Detection {
        walked: walked.len(),
        recorded: walked.iter().filter(|(_, r)| !r.is_empty()).count(),
        ..Detection::new(head.clone(), gates.clone())
    };

    for (key, points) in &series {
        d.trend
            .insert(key.clone(), points.iter().map(|p| p.instructions).collect());

        let steps: Vec<Step> = points
            .windows(2)
            .map(|w| step(key, walked, w[0], w[1]))
            .collect();
        let at_head = points.last().is_some_and(|p| p.at == head_at);

        match (at_head, steps.split_last()) {
            (true, Some((last, rest))) => {
                d.latest.push(last.clone());
                d.earlier
                    .extend(rest.iter().filter(|s| s.exceeds(gates)).cloned());
            }
            (true, None) => d.new_at_head.push(key.clone()),
            (false, _) => {
                d.missing_at_head.push(key.clone());
                d.earlier
                    .extend(steps.iter().filter(|s| s.exceeds(gates)).cloned());
            }
        }

        let gate = gates.get(&key.0, &key.1);
        if at_head && let Some(drift) = drift(key, walked, points, &steps, &gate) {
            d.drift.push(drift);
        }
    }

    // Oldest first reads as a history; series order within one commit stays
    // the stable key order.
    d.earlier
        .sort_by_key(|s| walked.iter().position(|(sha, _)| sha == &s.to));
    d
}

/// A cumulative rise beyond the series' gate made only of steps within it.
///
/// The same gate as the steps, floor included: drift is the rise the gate was
/// meant to catch arriving in pieces, so it is held to what one piece would
/// have been.
///
/// Measured from the last above-gate step, or the start of the window when
/// there was none. Starting after the step matters: a series that jumped 10%
/// and then crept another 2% would otherwise report that 10% twice, once as a
/// step and again as drift, and the creep after it — the part nobody has been
/// told about — would be buried in the total.
///
/// From the first point of that run rather than its minimum. "Since `abc`, up
/// 1.5%" is something a reader can check with one `tak compare`; a floor picked
/// out of the middle of the run is not.
fn drift(
    key: &Key,
    walked: &[(String, Vec<Record>)],
    points: &[Point],
    steps: &[Step],
    gate: &Gate,
) -> Option<Drift> {
    // steps[i] runs from points[i] to points[i + 1].
    let start = steps
        .iter()
        .rposition(|s| s.instructions.exceeds(gate))
        .map_or(0, |i| i + 1);
    let run = &points[start..];
    // Two points are one step, and that step is below the gate by construction.
    if run.len() < 3 {
        return None;
    }
    let total = step(key, walked, run[0], run[run.len() - 1]);
    if !total.instructions.exceeds(gate) {
        return None;
    }
    Some(Drift {
        key: key.clone(),
        since: total.from,
        points: run.len(),
        change: total.instructions,
    })
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// The commit a step belongs to, or the range when it cannot be narrowed.
///
/// Written as a git range so it can be pasted into `git log` as it stands:
/// `from..to` is exactly the commits that could have caused it.
fn span(s: &Step) -> String {
    if s.commits <= 1 {
        format!("`{}`", short(&s.to))
    } else {
        format!(
            "`{}..{}` ({} commits)",
            short(&s.from),
            short(&s.to),
            s.commits
        )
    }
}

fn instructions_cells(s: &Step, gates: &Gates) -> (String, String) {
    let ch = &s.instructions;
    // As in `compare`: the warning sign means "this fails", and a report-only
    // row that crossed its threshold says so in words instead.
    let flag = match (s.exceeds(gates), s.gate(gates).enabled) {
        (true, true) => " ⚠️",
        (true, false) => " (not gated)",
        (false, _) => "",
    };
    (
        format!("{} → {}", thousands(ch.base), thousands(ch.head)),
        format!("**{}**{flag}", signed_pct(ch.pct())),
    )
}

/// Render a detection as markdown, in the same shape as `compare`'s report so
/// the two read alike in a job summary.
pub fn markdown(d: &Detection, credit: bool) -> String {
    let gates = &d.gates;
    let global = gates.global;
    let uniform = d.uniform();
    // With every series at the global gate the prose names it once, as it did
    // before per-benchmark gates; otherwise each row carries its own.
    let the_gate = if uniform {
        format!("the {}% gate", global.pct)
    } else {
        "their gate".to_string()
    };
    let floor = if uniform && global.min_delta > 0 {
        format!(
            " (rises of {} instructions or fewer are not counted)",
            thousands(global.min_delta as f64)
        )
    } else {
        String::new()
    };
    let listed = |steps: &[&Step]| {
        steps
            .iter()
            .map(|s| {
                let mut out = format!("{} {}", describe(&s.key), signed_pct(s.instructions.pct()));
                if !uniform {
                    out.push_str(&format!(" (gate {})", s.gate(gates).threshold()));
                }
                out
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let head = short(&d.head);
    let mut out = format!(
        "Walked {} first-parent commit(s) ending at `{head}`, {} with measurements recorded.\n\n",
        d.walked, d.recorded
    );

    if d.nothing_compared() {
        let why = if d.new_at_head.is_empty() {
            "No instruction counts are recorded for this commit. Record it with \
             `tak run --record` before running `tak detect`."
        } else {
            "No series measured here has an earlier recorded point in the window: \
             this is the first recording, the history is too short or shallow, or \
             the runner class changed, which deliberately starts a new series."
        };
        if !d.empty_fails() {
            // Name the flag that waived it, so a report that passed says why.
            let flag = if d.no_gate {
                "--no-gate"
            } else {
                "--allow-empty"
            };
            out.push_str(&format!(
                "**Nothing was compared at `{head}`, and so nothing was gated** \
                 (`{flag}`). {why}\n"
            ));
        } else {
            out.push_str(&format!(
                "**Nothing was compared at `{head}`, so this check fails.** {why} \
                 When that is expected — the first recording, or the first on a new \
                 runner class — pass `--allow-empty`, or record an earlier commit on \
                 the same runner class first.\n"
            ));
        }
    } else {
        if uniform {
            out.push_str(
                "| benchmark | trend | step | instructions | Δ | wall (min) | Δ |\n\
                 |---|---|---|---:|---:|---:|---:|\n",
            );
        } else {
            out.push_str(
                "| benchmark | trend | step | instructions | Δ | gate | wall (min) | Δ |\n\
                 |---|---|---|---:|---:|---|---:|---:|\n",
            );
        }
        for s in &d.latest {
            let spark = d
                .trend
                .get(&s.key)
                .map(|v| sparkline(v))
                .filter(|v| !v.is_empty())
                .map(|v| format!("`{v}`"))
                .unwrap_or_else(|| "—".into());
            let (ins, ins_delta) = instructions_cells(s, gates);
            let gate_cell = if uniform {
                String::new()
            } else {
                format!(" {} |", s.gate(gates).describe())
            };
            let (wall, wall_delta) = match &s.wall {
                Some(w) => (
                    format!("{:.2} → {:.2}ms", w.base, w.head),
                    signed_pct(w.pct()),
                ),
                None => ("—".into(), "—".into()),
            };
            out.push_str(&format!(
                "| {} | {spark} | {} | {ins} | {ins_delta} |{gate_cell} {wall} | {wall_delta} |\n",
                describe(&s.key),
                span(s),
            ));
        }

        let failures = d.failures();
        let any_gated = d.latest.iter().any(|s| s.gate(gates).enabled);
        out.push('\n');
        if !any_gated {
            // "No step above the gate" is true of a report with nothing that
            // could fail, and reads as a pass. Say there was nothing to fail.
            out.push_str("Every benchmark here is report-only, so none can fail the gate.\n");
        } else if failures.is_empty() {
            if uniform {
                out.push_str(&format!(
                    "No instruction-count step above {}% at `{head}`{floor}.\n",
                    global.pct
                ));
            } else {
                out.push_str(&format!(
                    "No gated benchmark stepped beyond its gate at `{head}`.\n"
                ));
            }
        } else {
            out.push_str(&format!(
                "**{} benchmark(s) stepped above {the_gate} at `{head}`{floor}:** {}\n",
                failures.len(),
                listed(&failures)
            ));
            if failures.iter().any(|s| s.commits > 1) {
                out.push_str(
                    "\nA step shown as a range happened somewhere in commits that were \
                     not measured individually; `git log --first-parent` over that \
                     range lists the candidates.\n",
                );
            }
        }
        let reported = d.reported();
        if !reported.is_empty() {
            out.push_str(&format!(
                "\n**{} report-only benchmark(s) stepped above their gate at `{head}`, \
                 not failing:** {}\n",
                reported.len(),
                listed(&reported)
            ));
        }
    }

    if !d.earlier.is_empty() {
        out.push_str(&format!(
            "\n**Earlier steps above {the_gate}**, reported and not gated — \
             a step fails only the run for the commit that introduced it:\n\n"
        ));
        if uniform {
            out.push_str("| benchmark | step | instructions | Δ |\n|---|---|---:|---:|\n");
        } else {
            out.push_str(
                "| benchmark | step | instructions | Δ | gate |\n|---|---|---:|---:|---|\n",
            );
        }
        for s in &d.earlier {
            let (ins, delta) = instructions_cells(s, gates);
            let gate_cell = if uniform {
                String::new()
            } else {
                format!(" {} |", s.gate(gates).describe())
            };
            out.push_str(&format!(
                "| {} | {} | {ins} | {delta} |{gate_cell}\n",
                describe(&s.key),
                span(s)
            ));
        }
    }

    if !d.drift.is_empty() {
        out.push_str(&format!(
            "\n**Sustained drift**, reported and not gated — every step stayed within \
             {the_gate} and the total did not:\n\n"
        ));
        for dr in &d.drift {
            let gate = if uniform {
                String::new()
            } else {
                format!(" (gate {})", gates.get(&dr.key.0, &dr.key.1).describe())
            };
            out.push_str(&format!(
                "- {}: {} ({} → {}) over {} recorded commits since `{}`{gate}\n",
                describe(&dr.key),
                signed_pct(dr.change.pct()),
                thousands(dr.change.base),
                thousands(dr.change.head),
                dr.points,
                short(&dr.since),
            ));
        }
    }

    if !d.new_at_head.is_empty() {
        out.push_str(&format!(
            "\nNo earlier point in the window, nothing to compare against: {}\n",
            list(&d.new_at_head)
        ));
    }
    if !d.missing_at_head.is_empty() {
        out.push_str(&format!(
            "\nRecorded earlier in the window but not at `{head}` — a benchmark that \
             stops running also stops being checked: {}\n",
            list(&d.missing_at_head)
        ));
    }
    match d.cutoff {
        Some(Cutoff::Shallow) => out.push_str(
            "\nThis checkout is shallow, and the walk reached its boundary before \
             the window filled. Fetch more history (`fetch-depth: 0` with \
             `actions/checkout`) so earlier recordings are visible.\n",
        ),
        Some(Cutoff::ScanLimit) => out.push_str(&format!(
            "\nThe walk stopped at its limit of {} first-parent commits before the \
             window filled. Recordings older than that were not compared.\n",
            thousands(SCAN_LIMIT as f64)
        )),
        None => {}
    }

    out.push_str(
        "\n<sub>Only instruction counts gate, and only a step onto the newest commit. Wall clock \
         is shown for context — on identical hardware it moves 4-20% run to run.</sub>\n",
    );
    if credit {
        out.push_str(CREDIT);
    }
    out
}

fn list(keys: &[Key]) -> String {
    keys.iter().map(describe).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The global gate alone, as with no `tak.toml`.
    fn pct(p: f64) -> Gates {
        Gates::uniform(Gate::new(p, 0).unwrap())
    }

    fn rec(bench: &str, runner: &str, ins: f64) -> Record {
        Record {
            v: 1,
            bench: bench.into(),
            tool: "self".into(),
            version: None,
            runner: runner.into(),
            ts: "2026-01-01T00:00:00Z".into(),
            metrics: BTreeMap::from([
                (GATED_METRIC.to_string(), ins),
                (WALL_METRIC.to_string(), 10.0),
            ]),
        }
    }

    /// A walk named c0, c1, … oldest first, one `a`-on-`gha` value per entry;
    /// `None` is a commit with nothing recorded.
    fn walk(values: &[Option<f64>]) -> Vec<(String, Vec<Record>)> {
        values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    format!("c{i}"),
                    v.map(|v| vec![rec("a", "gha", v)]).unwrap_or_default(),
                )
            })
            .collect()
    }

    fn shas(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_step_onto_the_head_fails() {
        let d = analyze(
            &walk(&[Some(1000.0), Some(1000.0), Some(1100.0)]),
            &pct(1.0),
        );
        let f = d.failures();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].from.as_str(), f[0].to.as_str()), ("c1", "c2"));
        assert_eq!(f[0].commits, 1);
        assert!(d.earlier.is_empty());
    }

    #[test]
    fn a_step_below_the_gate_passes() {
        let d = analyze(&walk(&[Some(1000.0), Some(1005.0)]), &pct(1.0));
        assert!(d.failures().is_empty());
        assert_eq!(d.latest.len(), 1, "it is still reported");
    }

    #[test]
    fn an_improvement_never_fails() {
        let d = analyze(&walk(&[Some(1000.0), Some(500.0)]), &pct(1.0));
        assert!(d.failures().is_empty());
    }

    /// The point of gating only the head: the commit after a regression must
    /// pass, or a main workflow stays red until someone reverts the step.
    #[test]
    fn an_older_step_is_reported_not_failed() {
        let d = analyze(
            &walk(&[Some(1000.0), Some(1100.0), Some(1100.0)]),
            &pct(1.0),
        );
        assert!(d.failures().is_empty());
        assert_eq!(d.earlier.len(), 1);
        assert_eq!(d.earlier[0].to, "c1");
        assert!(markdown(&d, false).contains("Earlier steps above the 1% gate"));
    }

    /// Unrecorded commits between two points: the step can only be attributed
    /// to the range, and naming the later commit alone would blame one that may
    /// be innocent.
    #[test]
    fn a_step_across_unrecorded_commits_is_a_range() {
        let d = analyze(&walk(&[Some(1000.0), None, None, Some(1100.0)]), &pct(1.0));
        let f = d.failures();
        assert_eq!(f.len(), 1, "a range onto the head still fails");
        assert_eq!((f[0].from.as_str(), f[0].to.as_str()), ("c0", "c3"));
        assert_eq!(f[0].commits, 3);
        let md = markdown(&d, false);
        assert!(md.contains("`c0..c3` (3 commits)"), "{md}");
        assert!(md.contains("not measured individually"), "{md}");
    }

    /// Ranges are per series. A commit that recorded one benchmark and not
    /// another is a point for the first and part of a range for the second.
    #[test]
    fn ranges_are_per_series() {
        let mut w = walk(&[Some(1000.0), Some(1000.0), Some(1000.0)]);
        w[0].1.push(rec("b", "gha", 50.0));
        w[2].1.push(rec("b", "gha", 60.0));
        let d = analyze(&w, &pct(1.0));
        let b = d.latest.iter().find(|s| s.key.0 == "b").unwrap();
        assert_eq!(b.commits, 2);
        let a = d.latest.iter().find(|s| s.key.0 == "a").unwrap();
        assert_eq!(a.commits, 1);
        assert_eq!(d.failures().len(), 1);
    }

    /// A runner change starts a new series; it is never a step.
    #[test]
    fn runners_never_line_up() {
        let w = vec![
            ("c0".to_string(), vec![rec("a", "gha-linux", 1000.0)]),
            ("c1".to_string(), vec![rec("a", "gha-macos", 9000.0)]),
        ];
        let d = analyze(&w, &pct(1.0));
        assert!(d.failures().is_empty());
        assert!(d.nothing_compared());
        assert_eq!(d.new_at_head.len(), 1);
        assert_eq!(d.missing_at_head.len(), 1);
        let md = markdown(&d, false);
        assert!(md.contains("Nothing was compared"), "{md}");
        assert!(md.contains("gha-linux") && md.contains("gha-macos"), "{md}");
    }

    /// Two runners in the same walk are two independent series, each stepping
    /// against itself only.
    #[test]
    fn runners_are_partitioned_within_a_walk() {
        let w = vec![
            (
                "c0".to_string(),
                vec![rec("a", "fast", 1000.0), rec("a", "slow", 5000.0)],
            ),
            (
                "c1".to_string(),
                vec![rec("a", "fast", 1000.0), rec("a", "slow", 5200.0)],
            ),
        ];
        let d = analyze(&w, &pct(1.0));
        let f = d.failures();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].key.2, "slow");
        assert_eq!(f[0].instructions.base, 5000.0, "never the other runner's");
    }

    #[test]
    fn creeping_sub_threshold_steps_are_drift() {
        let d = analyze(
            &walk(&[Some(1000.0), Some(1004.0), Some(1008.0), Some(1012.0)]),
            &pct(1.0),
        );
        assert!(d.failures().is_empty(), "drift never fails");
        assert!(d.earlier.is_empty());
        assert_eq!(d.drift.len(), 1);
        assert_eq!(d.drift[0].since, "c0");
        assert_eq!(d.drift[0].points, 4);
        let md = markdown(&d, false);
        assert!(md.contains("Sustained drift"), "{md}");
        assert!(md.contains("+1.20%"), "{md}");
    }

    #[test]
    fn a_flat_series_is_not_drift() {
        let d = analyze(&walk(&[Some(1000.0), Some(1003.0), Some(999.0)]), &pct(1.0));
        assert!(d.drift.is_empty());
    }

    /// Drift is measured from after the last real step, so the step is not
    /// counted twice and the creep since it is what gets reported.
    #[test]
    fn drift_starts_after_the_last_step() {
        let d = analyze(
            &walk(&[
                Some(1000.0),
                Some(1100.0),
                Some(1105.0),
                Some(1110.0),
                Some(1115.0),
            ]),
            &pct(1.0),
        );
        assert_eq!(d.earlier.len(), 1);
        assert_eq!(d.drift.len(), 1);
        assert_eq!(d.drift[0].since, "c1");
        assert_eq!(d.drift[0].change.base, 1100.0);

        // Without the creep, the step alone is not drift.
        let d = analyze(
            &walk(&[Some(1000.0), Some(1100.0), Some(1100.0)]),
            &pct(1.0),
        );
        assert!(d.drift.is_empty());
    }

    /// A head with nothing recorded compares nothing. It must say so rather
    /// than read like a clean pass.
    #[test]
    fn an_unrecorded_head_says_so() {
        let d = analyze(&walk(&[Some(1000.0), Some(1000.0), None]), &pct(1.0));
        assert!(d.nothing_compared());
        assert!(d.failures().is_empty());
        let md = markdown(&d, false);
        assert!(md.contains("No instruction counts are recorded"), "{md}");
        assert!(md.contains("not at `c2`"), "{md}");
    }

    #[test]
    fn a_filled_window_is_never_cut_short() {
        let shallow = || -> Result<bool> { panic!("no need to ask") };
        assert_eq!(cutoff(20, 20, true, shallow).unwrap(), None);
    }

    /// History that goes on past the walk has to be reported, not read as the
    /// end of the project.
    #[test]
    fn hitting_the_scan_limit_is_a_cutoff() {
        let shallow = || -> Result<bool> { panic!("the limit decides first") };
        assert_eq!(
            cutoff(3, 20, true, shallow).unwrap(),
            Some(Cutoff::ScanLimit)
        );
    }

    /// Fewer commits than the limit is the end of the available history:
    /// a shallow boundary when the clone is shallow, the real root otherwise.
    #[test]
    fn the_end_of_history_is_a_cutoff_only_when_shallow() {
        assert_eq!(
            cutoff(3, 20, false, || Ok(true)).unwrap(),
            Some(Cutoff::Shallow)
        );
        assert_eq!(cutoff(3, 20, false, || Ok(false)).unwrap(), None);
    }

    #[test]
    fn each_cutoff_is_named_in_the_report() {
        let mut d = analyze(&walk(&[Some(1000.0), Some(1000.0)]), &pct(1.0));
        d.cutoff = Some(Cutoff::ScanLimit);
        let md = markdown(&d, false);
        assert!(md.contains("limit of 10,000 first-parent commits"), "{md}");
        d.cutoff = Some(Cutoff::Shallow);
        assert!(markdown(&d, false).contains("This checkout is shallow"));
        d.cutoff = None;
        let md = markdown(&d, false);
        assert!(!md.contains("shallow") && !md.contains("limit of"), "{md}");
    }

    /// Comparing nothing fails unless it was explicitly accepted, and the
    /// report says which of the two happened and what to do about it.
    #[test]
    fn nothing_compared_fails_unless_allowed() {
        let mut d = analyze(&walk(&[Some(1000.0)]), &pct(1.0));
        assert!(d.nothing_compared());
        assert!(d.empty_fails(), "empty is a failure by default");
        let md = markdown(&d, false);
        assert!(md.contains("so this check fails.**"), "{md}");
        assert!(md.contains("pass `--allow-empty`"), "{md}");
        assert!(!md.contains("nothing was gated"), "{md}");

        d.allow_empty = true;
        assert!(!d.empty_fails());
        let md = markdown(&d, false);
        assert!(md.contains("nothing was gated** (`--allow-empty`)"), "{md}");
        assert!(!md.contains("this check fails"), "{md}");
    }

    /// `--no-gate` means never fail, so it waives an empty comparison too, and
    /// the report names it rather than `--allow-empty`.
    #[test]
    fn no_gate_waives_an_empty_comparison() {
        let mut d = analyze(&walk(&[Some(1000.0)]), &pct(1.0));
        d.no_gate = true;
        assert!(!d.empty_fails());
        let md = markdown(&d, false);
        assert!(md.contains("nothing was gated** (`--no-gate`)"), "{md}");
        assert!(!md.contains("this check fails"), "{md}");
    }

    /// A comparison that happened is never an empty one, whatever the flag.
    #[test]
    fn a_real_comparison_is_not_empty() {
        let d = analyze(&walk(&[Some(1000.0), Some(1000.0)]), &pct(1.0));
        assert!(!d.empty_fails());
    }

    /// Only instruction counts may drive a failure, however much wall clock
    /// moved. A series with no instruction counts is not examined at all.
    #[test]
    fn wall_clock_never_gates() {
        let mut w = walk(&[Some(1000.0), Some(1000.0)]);
        w[1].1[0].metrics.insert(WALL_METRIC.into(), 1000.0);
        for (at, wall) in [(0, 1.0), (1, 100.0)] {
            let mut r = rec("w", "gha", 0.0);
            r.metrics = BTreeMap::from([(WALL_METRIC.to_string(), wall)]);
            w[at].1.push(r);
        }
        let d = analyze(&w, &pct(0.001));
        assert!(d.failures().is_empty());
        assert!(d.latest.iter().all(|s| s.key.0 == "a"));
    }

    /// A re-run's noisy duplicate collapses to the minimum, as in `compare`.
    #[test]
    fn duplicates_reduce_to_the_minimum() {
        let mut w = walk(&[Some(1000.0), Some(1000.0)]);
        w[1].1.push(rec("a", "gha", 3000.0));
        assert!(analyze(&w, &pct(1.0)).failures().is_empty());
    }

    #[test]
    fn a_rise_from_zero_fails() {
        let d = analyze(&walk(&[Some(0.0), Some(5.0)]), &pct(1.0));
        assert_eq!(d.failures().len(), 1);
    }

    #[test]
    fn an_empty_walk_is_nothing_compared() {
        let d = analyze(&[], &pct(1.0));
        assert!(d.nothing_compared());
        assert!(markdown(&d, false).contains("Only instruction counts gate"));
    }

    #[test]
    fn the_window_counts_recorded_commits() {
        let newest_first = shas(&["h", "x1", "r1", "x2", "r2", "r3", "x3"]);
        let annotated: BTreeSet<String> = shas(&["h", "r1", "r2", "r3"]).into_iter().collect();
        assert_eq!(
            select(&newest_first, &annotated, 3),
            shas(&["r2", "x2", "r1", "x1", "h"])
        );
    }

    /// The head belongs to the walk even with nothing recorded, and history
    /// older than the oldest recorded commit does not.
    #[test]
    fn the_head_is_always_walked() {
        let newest_first = shas(&["h", "x1", "r1", "x2"]);
        let annotated: BTreeSet<String> = shas(&["r1"]).into_iter().collect();
        assert_eq!(
            select(&newest_first, &annotated, 20),
            shas(&["r1", "x1", "h"])
        );
        assert_eq!(select(&shas(&["h"]), &BTreeSet::new(), 20), shas(&["h"]));
        assert!(select(&[], &BTreeSet::new(), 20).is_empty());
    }

    #[test]
    fn the_trend_covers_the_window() {
        let d = analyze(&walk(&[Some(1.0), None, Some(2.0), Some(3.0)]), &pct(1.0));
        let key = ("a".to_string(), "self".to_string(), "gha".to_string());
        assert_eq!(d.trend[&key], vec![1.0, 2.0, 3.0]);
    }

    /// A 1% global gate, with benchmark `a` given its own.
    fn gates_with_a(a_pct: f64, min_delta: u64, enabled: bool) -> Gates {
        let mut g = pct(1.0);
        g.set_bench(
            "a",
            Gate {
                pct: a_pct,
                min_delta,
                enabled,
            },
        );
        g
    }

    /// `a` and `b` both step +10% onto the head.
    fn two_series_step() -> Vec<(String, Vec<Record>)> {
        vec![
            (
                "c0".to_string(),
                vec![rec("a", "gha", 1000.0), rec("b", "gha", 1000.0)],
            ),
            (
                "c1".to_string(),
                vec![rec("a", "gha", 1100.0), rec("b", "gha", 1100.0)],
            ),
        ]
    }

    /// Each series is held to its own gate: `a`'s 20% lets its 10% step
    /// through while `b`, at the global 1%, fails on the same step.
    #[test]
    fn a_step_is_held_to_its_own_series_gate() {
        let d = analyze(&two_series_step(), &gates_with_a(20.0, 0, true));
        let f = d.failures();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].key.0, "b");
        assert!(!d.uniform());
        let md = markdown(&d, false);
        assert!(md.contains("| gate |"), "{md}");
        assert!(md.contains("stepped above their gate"), "{md}");
        assert!(md.contains("(gate 1%)"), "{md}");
    }

    /// The floor applies as well: a 100-instruction step is 10% of `a`, far
    /// past its percentage, and still within a 500-instruction floor.
    #[test]
    fn a_step_within_its_floor_passes() {
        let d = analyze(&two_series_step(), &gates_with_a(1.0, 500, true));
        let f = d.failures();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].key.0, "b");
        assert!(markdown(&d, false).contains("floor 500"));
    }

    /// A report-only benchmark is examined like any other and flagged when it
    /// crosses its threshold, but it can never fail the command.
    #[test]
    fn a_report_only_step_is_reported_and_never_fails() {
        let d = analyze(&two_series_step(), &gates_with_a(1.0, 0, false));
        let f = d.failures();
        assert_eq!(f.len(), 1, "only the gated one fails");
        assert_eq!(f[0].key.0, "b");
        assert_eq!(d.reported().len(), 1);
        assert_eq!(d.reported()[0].key.0, "a");
        let md = markdown(&d, false);
        assert!(md.contains("**+10.00%** (not gated)"), "{md}");
        assert!(md.contains("report only (1%)"), "{md}");
        assert!(
            md.contains("1 report-only benchmark(s) stepped above their gate"),
            "{md}"
        );

        // With nothing gated at all, the verdict says so rather than passing.
        let mut all = gates_with_a(1.0, 0, false);
        all.set_bench(
            "b",
            Gate {
                pct: 1.0,
                min_delta: 0,
                enabled: false,
            },
        );
        let d = analyze(&two_series_step(), &all);
        assert!(d.failures().is_empty());
        assert!(!d.empty_fails());
        assert!(markdown(&d, false).contains("Every benchmark here is report-only"));
    }

    /// An earlier step on a report-only benchmark is listed like any other.
    #[test]
    fn earlier_steps_use_the_series_gate() {
        let w = walk(&[Some(1000.0), Some(1100.0), Some(1100.0)]);
        assert_eq!(analyze(&w, &gates_with_a(20.0, 0, true)).earlier.len(), 0);
        assert_eq!(analyze(&w, &gates_with_a(1.0, 0, false)).earlier.len(), 1);
    }

    /// Drift is held to the series' gate too: a 1.2% creep is drift at the
    /// global 1%, not under `a`'s own 5%, and not within a floor of 50.
    #[test]
    fn drift_uses_the_series_gate() {
        let w = walk(&[Some(1000.0), Some(1004.0), Some(1008.0), Some(1012.0)]);
        assert_eq!(analyze(&w, &pct(1.0)).drift.len(), 1);
        assert!(analyze(&w, &gates_with_a(5.0, 0, true)).drift.is_empty());
        assert!(analyze(&w, &gates_with_a(1.0, 50, true)).drift.is_empty());

        // Report-only drift is still reported: reporting is all drift does.
        let d = analyze(&w, &gates_with_a(1.0, 0, false));
        assert_eq!(d.drift.len(), 1);
        assert!(markdown(&d, false).contains("(gate report only (1%))"));
    }

    /// With every series at the global gate, the report reads as it did
    /// before per-benchmark gates: no gate column, one `N%` in the prose.
    #[test]
    fn a_uniform_report_keeps_its_wording() {
        let d = analyze(&two_series_step(), &pct(1.0));
        assert!(d.uniform());
        let md = markdown(&d, false);
        assert!(!md.contains("| gate |"), "{md}");
        assert!(md.contains("stepped above the 1% gate"), "{md}");
    }
}
