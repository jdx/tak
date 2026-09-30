//! A benchmark's history: each series' measurements over first-parent commits.
//!
//! `tak compare` answers whether one change moved anything; this answers how a
//! number got to where it is. The two are read together — a value that has
//! drifted for weeks and a value one commit just moved look identical in a
//! two-column comparison, and nothing alike here.
//!
//! Everything in this module is pure, over commits already read by
//! [`crate::notes::log`], so both renderings are tested without a repository.
//! It deliberately detects nothing: it draws what was recorded and leaves the
//! judgement of what counts as a step to the reader and to the gate.

use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::compare::{self, GATED_METRIC, Key, WALL_METRIC};
use crate::config::SELF_TOOL;
use crate::notes::Logged;
use crate::record::Record;

/// One series' values on one recorded commit.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    /// Index into [`History::commits`].
    pub commit: usize,
    pub instructions: Option<f64>,
    pub wall_min_ms: Option<f64>,
}

/// One benchmark, on one tool, on one runner class, oldest point first.
///
/// Runner is in the key for the reason it is everywhere else: a line that
/// crossed from one machine class to another would draw an infrastructure
/// change as a code change.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub key: Key,
    pub points: Vec<Point>,
}

impl Series {
    /// Each instruction-count point with its change from the previous one that
    /// has an instruction count, oldest first.
    ///
    /// The previous *measured* point, not the previous commit: a series is
    /// often absent from a commit — a benchmark added later, a run that
    /// failed — and the gap says nothing about which way the number moved.
    fn deltas(&self) -> Vec<Option<f64>> {
        let mut prev: Option<f64> = None;
        self.points
            .iter()
            .map(|p| {
                let d = match (prev, p.instructions) {
                    (Some(base), Some(head)) => pct(base, head),
                    _ => None,
                };
                if p.instructions.is_some() {
                    prev = p.instructions;
                }
                d
            })
            .collect()
    }
}

/// Change as a percentage, `None` against zero — the rule
/// [`compare::Change::pct`] uses, for the same reason: a percentage of nothing
/// is undefined, and printing `+0.00%` would say the opposite of what happened.
fn pct(base: f64, head: f64) -> Option<f64> {
    (base != 0.0).then(|| (head - base) / base * 100.0)
}

/// What a walk found, reduced to series.
#[derive(Debug, Clone)]
pub struct History {
    /// Recorded commits in the window, oldest first. Commits with nothing drawable
    /// recorded are not here: most commits on a trunk that records push tips
    /// have no measurement, and a row each would bury the ones that do.
    pub commits: Vec<Logged>,
    pub series: Vec<Series>,
    /// First-parent commits walked, recorded or not.
    pub walked: usize,
    /// Recorded commits exist further back than the limit reached. Whether,
    /// not how many: counting them meant reading every note on the trunk, and
    /// [`Enough`] stops the walk at the first one instead.
    pub older: bool,
    /// Commits whose records carry neither metric this report draws. Said
    /// aloud when nothing else was found, so an empty report does not claim
    /// that nothing was recorded when something was.
    pub undrawable: usize,
    /// The clone is shallow, so the walk may have stopped short of the project's
    /// first commit rather than at it.
    pub shallow: bool,
}

/// Reduce a first-parent walk, newest first, to at most `limit` recorded
/// commits and the series across them.
///
/// `benches`, when non-empty, keeps only those benchmarks, and a commit counts
/// as recorded only if one of them is on it — otherwise `-n 20 --bench x`
/// could show three points because seventeen of the twenty carried only other
/// benchmarks. A name that appears nowhere in the walk is an error rather than
/// an empty report: a typo in a CI step would otherwise publish a blank page
/// every day and nobody would notice.
///
/// Several records for one series on one commit reduce to the minimum, through
/// the same [`compare::index`] the comparison uses, so a chart and the
/// comparison it sits beside can never disagree about a commit's value.
pub fn build(
    walked: Vec<Logged>,
    limit: usize,
    benches: &[String],
    shallow: bool,
) -> Result<History> {
    let walked_n = walked.len();
    if !benches.is_empty() {
        check_benches(&walked, benches)?;
    }

    let mut undrawable = 0;
    let mut recorded: Vec<Logged> = walked
        .into_iter()
        .filter_map(|mut c| {
            c.records
                .retain(|r| benches.is_empty() || benches.contains(&r.bench));
            let had_records = !c.records.is_empty();
            // A record with neither metric drawn here — only custom ones, say
            // — is dropped before counting. Left in, it made its commit use
            // up one of the `-n` slots while contributing no point, so enough
            // of them could push every drawable measurement out of the window.
            c.records.retain(drawable);
            if had_records && c.records.is_empty() {
                undrawable += 1;
            }
            (!c.records.is_empty()).then_some(c)
        })
        .collect();
    let older = recorded.len() > limit;
    recorded.truncate(limit);
    // The walk is newest first; a series reads oldest to newest.
    recorded.reverse();

    let mut series: BTreeMap<Key, Vec<Point>> = BTreeMap::new();
    for (i, c) in recorded.iter().enumerate() {
        let values = compare::index(&c.records);
        let keys: BTreeSet<&Key> = values.keys().map(|(k, _)| k).collect();
        for key in keys {
            let get = |metric: &str| values.get(&(key.clone(), metric.to_string())).copied();
            // Every record left carries one of the two, so every key has a
            // value to draw.
            series.entry(key.clone()).or_default().push(Point {
                commit: i,
                instructions: get(GATED_METRIC),
                wall_min_ms: get(WALL_METRIC),
            });
        }
    }

    Ok(History {
        commits: recorded,
        series: series
            .into_iter()
            .map(|(key, points)| Series { key, points })
            .collect(),
        walked: walked_n,
        older,
        undrawable,
        shallow,
    })
}

/// Decides when a newest-first walk has read far enough for [`build`] to
/// produce the report the whole walk would.
///
/// That is one recorded commit past the limit, which is proof that older ones
/// exist, and every benchmark named with `--bench` seen with something to
/// draw, so that [`check_benches`] passes on the prefix exactly when it would
/// on the whole. Until both hold, the walk goes on to the end, and the report
/// is built from all of it as before.
///
/// "Recorded" is [`build`]'s rule, not merely a note being present, or a run of
/// commits carrying only other benchmarks would end the walk before the
/// selected one's points were reached.
pub struct Enough<'a> {
    limit: usize,
    benches: &'a [String],
    recorded: usize,
    drawn: BTreeSet<String>,
}

impl<'a> Enough<'a> {
    pub fn new(limit: usize, benches: &'a [String]) -> Self {
        Enough {
            limit,
            benches,
            recorded: 0,
            drawn: BTreeSet::new(),
        }
    }

    /// Take in the next commit of the walk; true once nothing older can change
    /// the report.
    pub fn after(&mut self, c: &Logged) -> bool {
        let mut counted = false;
        for r in c.records.iter().filter(|r| drawable(r)) {
            if self.benches.is_empty() || self.benches.contains(&r.bench) {
                counted = true;
                self.drawn.insert(r.bench.clone());
            }
        }
        self.recorded += usize::from(counted);
        self.recorded > self.limit && self.benches.iter().all(|b| self.drawn.contains(b))
    }
}

/// Whether a record carries a metric this report draws.
fn drawable(r: &Record) -> bool {
    r.metrics.contains_key(GATED_METRIC) || r.metrics.contains_key(WALL_METRIC)
}

/// Every benchmark named with `--bench` must have something to draw somewhere
/// in the walk.
///
/// Two separate failures, because they have separate fixes. A name recorded
/// nowhere is usually a typo. A name recorded only with other metrics is a
/// benchmark that stopped producing — or never produced — an instruction
/// count or a wall time, and filtering it would otherwise yield a report that
/// succeeds, says nothing was recorded, and gets published blank.
///
/// The whole walk rather than the `-n` window is the right scope: `-n` counts
/// only commits with something drawable, so if any exist the window reaches
/// back to them.
fn check_benches(walked: &[Logged], benches: &[String]) -> Result<()> {
    let records = || walked.iter().flat_map(|c| c.records.iter());
    let seen: BTreeSet<&str> = records().map(|r| r.bench.as_str()).collect();
    let drawn: BTreeSet<&str> = records()
        .filter(|r| drawable(r))
        .map(|r| r.bench.as_str())
        .collect();
    let named = |names: Vec<&String>| {
        names
            .iter()
            .map(|b| format!("`{b}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let missing: Vec<&String> = benches
        .iter()
        .filter(|b| !seen.contains(b.as_str()))
        .collect();
    if !missing.is_empty() {
        bail!(
            "no measurements of {} in {} commit(s) of history (recorded: {})",
            named(missing),
            walked.len(),
            if seen.is_empty() {
                "none".to_string()
            } else {
                seen.into_iter().collect::<Vec<_>>().join(", ")
            }
        );
    }
    let blank: Vec<&String> = benches
        .iter()
        .filter(|b| !drawn.contains(b.as_str()))
        .collect();
    if !blank.is_empty() {
        bail!(
            "{} has no {GATED_METRIC} or {WALL_METRIC} measurements in {} commit(s) of \
             history; its records carry only other metrics, which `tak log` does not show",
            named(blank),
            walked.len()
        );
    }
    Ok(())
}

/// `abcdef0123…` -> `abcdef0`, git's own default abbreviation.
fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// The calendar date of an ISO 8601 timestamp.
fn day(date: &str) -> &str {
    &date[..date.len().min(10)]
}

/// Subjects longer than this are cut. The subject is the last column precisely
/// so that it can be ragged, but a 200-character one still wraps every row.
const SUBJECT_CHARS: usize = 72;

fn clip(s: &str) -> String {
    if s.chars().count() <= SUBJECT_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(SUBJECT_CHARS - 1).collect();
    out.push('…');
    out
}

/// A sentence saying what the walk covered, and why it may have covered less
/// than was asked for. Shared by both renderings so they cannot disagree.
fn coverage(h: &History, rev: &str) -> String {
    let mut out = if h.commits.is_empty() && h.undrawable > 0 {
        format!(
            "No {GATED_METRIC} or {WALL_METRIC} measurements on the first-parent history of \
             `{rev}` ({} commit(s) walked; {} carried only other metrics, which this report \
             does not show).",
            h.walked, h.undrawable
        )
    } else if h.commits.is_empty() {
        format!(
            "No measurements recorded on the first-parent history of `{rev}` ({} commit(s) walked).",
            h.walked
        )
    } else {
        format!(
            "{} recorded commit(s) on the first-parent history of `{rev}`, {} to {} ({} commit(s) walked).",
            h.commits.len(),
            day(&h.commits[0].date),
            day(&h.commits[h.commits.len() - 1].date),
            h.walked
        )
    };
    if h.older {
        out.push_str(" Older recorded commits are not shown; `-n` shows more.");
    } else if h.shallow {
        // Only worth saying when the limit was not what stopped the walk.
        out.push_str(
            " This clone is shallow, so older measurements may exist beyond where the \
             walk stopped; `git fetch --unshallow` reaches them.",
        );
    }
    out
}

/// The caveat every rendering carries: the numbers beside the gated one are
/// context, and the wrong one to act on.
const WALL_CAVEAT: &str = "Only instruction counts gate. Wall clock is shown for context — on \
     identical hardware it moves 4-20% run to run.";

/// A markdown table whose columns are padded to line up.
///
/// Padded because this is read in a terminal at least as often as rendered,
/// and padding changes nothing once it is.
fn table(header: &[&str], right: &[bool], rows: &[Vec<String>]) -> String {
    let mut width: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in width.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<String>| -> String {
        let mut s = String::from("|");
        for (i, cell) in cells.iter().enumerate() {
            let pad = width[i].saturating_sub(cell.chars().count());
            // The last column is ragged: trailing spaces on the widest free
            // text column only lengthen every line.
            if i + 1 == cells.len() && !right[i] {
                let _ = write!(s, " {cell} |");
            } else if right[i] {
                let _ = write!(s, " {}{cell} |", " ".repeat(pad));
            } else {
                let _ = write!(s, " {cell}{} |", " ".repeat(pad));
            }
        }
        s.push('\n');
        s
    };
    let mut out = line(header.iter().map(|h| h.to_string()).collect());
    out.push('|');
    for (i, w) in width.iter().enumerate() {
        // The ragged column's rule matches its header, not its widest cell.
        let w = if i + 1 == width.len() && !right[i] {
            header[i].chars().count().max(1)
        } else {
            *w
        };
        // Two wider than the column, for the spaces either side of a cell.
        let dashes = "-".repeat(w + 2);
        if right[i] {
            let _ = write!(out, "{}:|", &dashes[1..]);
        } else {
            let _ = write!(out, "{dashes}|");
        }
    }
    out.push('\n');
    for row in rows {
        out.push_str(&line(row.clone()));
    }
    out
}

/// Rows for one series, newest first — the order `git log` reads in, and the
/// one where the most recent number is the first thing seen. Each row comes
/// with its commit, so a renderer can link the abbreviated SHA it prints.
fn rows<'h>(h: &'h History, s: &Series) -> Vec<(&'h Logged, Vec<String>)> {
    let deltas = s.deltas();
    s.points
        .iter()
        .zip(deltas)
        .rev()
        .map(|(p, d)| {
            let c = &h.commits[p.commit];
            let cells = vec![
                short(&c.sha).to_string(),
                day(&c.date).to_string(),
                p.instructions
                    .map(compare::thousands)
                    .unwrap_or_else(|| "—".into()),
                match (p.instructions, d) {
                    (Some(_), Some(d)) => compare::signed_pct(Some(d)),
                    _ => "—".into(),
                },
                p.wall_min_ms
                    .map(|w| format!("{w:.2}ms"))
                    .unwrap_or_else(|| "—".into()),
                clip(&c.subject),
            ];
            (c, cells)
        })
        .collect()
}

const HEADER: [&str; 6] = [
    "commit",
    "date",
    "instructions",
    "Δ",
    "wall (min)",
    "subject",
];
const RIGHT: [bool; 6] = [false, false, true, true, true, false];

/// Render the history as markdown: one table per series.
///
/// Markdown for the same reason as [`compare::markdown`] — readable unrendered,
/// and it drops straight into `$GITHUB_STEP_SUMMARY`.
pub fn markdown(h: &History, rev: &str, credit: bool) -> String {
    let mut out = coverage(h, rev);
    out.push('\n');
    for s in &h.series {
        let _ = write!(out, "\n### {}\n\n", compare::describe(&s.key));
        // A subject is free text; an unescaped pipe in one splits its row.
        let body: Vec<Vec<String>> = rows(h, s)
            .into_iter()
            .map(|(_, mut r)| {
                r[0] = format!("`{}`", r[0]);
                let last = r.len() - 1;
                r[last] = compare::cell(&r[last]);
                r
            })
            .collect();
        out.push_str(&table(&HEADER, &RIGHT, &body));
    }
    if !h.series.is_empty() {
        let _ = write!(out, "\n<sub>{WALL_CAVEAT}</sub>\n");
    }
    if credit {
        out.push_str(compare::CREDIT);
    }
    out
}

/// Escape text for HTML element content and quoted attribute values alike.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Chart geometry, in SVG user units. The page scales the drawing to its
/// column, so these fix proportions and label sizes rather than pixels.
const WIDTH: f64 = 720.0;
const LEFT: f64 = 76.0;
const RIGHT_PAD: f64 = 12.0;
/// Plot heights. Wall time's is smaller on purpose: it is the context, and
/// should not take the same space as the number that gates.
const INS_PLOT: f64 = 150.0;
const WALL_PLOT: f64 = 56.0;
/// Room above the plot for the chart's name.
const TOP: f64 = 24.0;
/// Room below: enough for commit labels on the chart that carries them, and a
/// sliver on the one that does not.
const AXIS_BOTTOM: f64 = 28.0;
const BARE_BOTTOM: f64 = 8.0;

/// Closer together than this, per-commit dots merge into a smear; the line
/// alone carries the shape, and the hover columns still identify each commit.
///
/// Spacing rather than a count of the series' own points, because every
/// series shares the page's commit axis: a benchmark added recently has few
/// points, all of them crowded into the right-hand end.
const MIN_DOT_SPACING: f64 = 12.0;

/// The smallest vertical span a chart is drawn over, as a fraction of its
/// values' midpoint.
///
/// Scaling each series to its own range is necessary — a 7M-instruction
/// startup benchmark and a 100M install benchmark share no useful axis — but a
/// range fitted tightly to a deterministic metric draws its ~0.02% run-to-run
/// wobble as full-height swings. With a floor of 2%, that wobble stays flat and
/// a step the size of the default 1% gate still fills half the chart.
const INS_MIN_SPAN: f64 = 0.02;
/// The same floor for wall time, wide because the metric is: 4-20% run to run
/// on a quiet host. A tighter one would draw that noise as a trend.
const WALL_MIN_SPAN: f64 = 0.25;

/// The y range a series is drawn over: its values, widened to at least
/// `min_span` of their midpoint, with a margin so no point sits on an edge.
///
/// A flat series lands at mid-height, for the reason [`compare::sparkline`]
/// gives: a deterministic number that never moved is the best outcome, and
/// drawing it at the floor would make it look like the worst.
fn domain(values: &[f64], min_span: f64) -> (f64, f64) {
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mid = (lo + hi) / 2.0;
    // `1.0` keeps an all-zero series from dividing by a zero span.
    let span = (hi - lo).max(mid.abs() * min_span).max(1.0);
    let margin = span * 0.1;
    let (mut a, mut b) = (mid - span / 2.0 - margin, mid + span / 2.0 + margin);
    // Counts and durations are never negative; an axis that goes below zero
    // implies they could.
    if a < 0.0 && lo >= 0.0 {
        b -= a;
        a = 0.0;
    }
    (a, b)
}

/// A value to four significant figures with an SI suffix: `29.41M`.
///
/// Four because the narrowest chart spans 2% of its midpoint, and three
/// figures would print the top and bottom labels of such a chart identically.
fn si(v: f64) -> String {
    let (scaled, suffix) = match v.abs() {
        a if a >= 1e9 => (v / 1e9, "G"),
        a if a >= 1e6 => (v / 1e6, "M"),
        a if a >= 1e3 => (v / 1e3, "k"),
        _ => (v, ""),
    };
    let digits = if scaled.abs() >= 100.0 {
        1
    } else if scaled.abs() >= 10.0 {
        2
    } else {
        3
    };
    format!("{scaled:.digits$}{suffix}")
}

/// Horizontal position of a commit. Every chart on the page shares this axis,
/// so a series that starts later starts further right, and a commit sits in
/// the same column in every chart.
fn x_of(i: usize, n: usize) -> f64 {
    let plot = WIDTH - LEFT - RIGHT_PAD;
    if n <= 1 {
        LEFT + plot / 2.0
    } else {
        LEFT + plot * i as f64 / (n - 1) as f64
    }
}

/// What a point says on hover: which commit, and everything measured on it.
fn tooltip(c: &Logged, p: &Point, delta: Option<f64>) -> String {
    let mut t = format!("{} · {}\n{}", short(&c.sha), day(&c.date), c.subject);
    if let Some(v) = p.instructions {
        let _ = write!(t, "\ninstructions: {}", compare::thousands(v));
        if let Some(d) = delta {
            let _ = write!(t, " ({})", compare::signed_pct(Some(d)));
        }
    }
    if let Some(w) = p.wall_min_ms {
        let _ = write!(t, "\nwall (min): {w:.2} ms");
    }
    t
}

/// Where a commit's page is, when the repository lives somewhere that has one.
fn commit_url(repo: Option<&str>, sha: &str) -> Option<String> {
    repo.map(|r| format!("https://github.com/{r}/commit/{sha}"))
}

/// Which metric a chart draws.
#[derive(Clone, Copy)]
enum Metric {
    Instructions,
    Wall,
}

/// One chart: a metric over the shared commit axis. `x_axis` labels the first
/// and last commit beneath it, and is set on whichever chart is drawn last in
/// a series, so the labels sit once under the pair rather than between them.
fn chart(h: &History, s: &Series, metric: Metric, repo: Option<&str>, x_axis: bool) -> String {
    let (plot, min_span, class, label) = match metric {
        Metric::Instructions => (INS_PLOT, INS_MIN_SPAN, "ins", "instructions"),
        Metric::Wall => (
            WALL_PLOT,
            WALL_MIN_SPAN,
            "wall",
            "wall (min), ms · context only, never gates",
        ),
    };
    let bottom = TOP + plot;
    let height = bottom + if x_axis { AXIS_BOTTOM } else { BARE_BOTTOM };
    let value = |p: &Point| match metric {
        Metric::Instructions => p.instructions,
        Metric::Wall => p.wall_min_ms,
    };
    let n = h.commits.len();
    let pts: Vec<(usize, f64)> = s
        .points
        .iter()
        .filter_map(|p| value(p).map(|v| (p.commit, v)))
        .collect();
    let values: Vec<f64> = pts.iter().map(|(_, v)| *v).collect();
    let (lo, hi) = domain(&values, min_span);
    let y_of = |v: f64| bottom - (v - lo) / (hi - lo) * (bottom - TOP);
    let fmt_tick = |v: f64| match metric {
        Metric::Instructions => si(v),
        Metric::Wall => format!("{v:.1}"),
    };

    let mut svg = String::new();
    let _ = write!(
        svg,
        "<svg class=\"chart {class}-chart\" viewBox=\"0 0 {WIDTH} {height}\" role=\"img\" \
         aria-label=\"{} for {}, {} point(s)\">",
        esc(label),
        esc(&compare::describe(&s.key).replace('`', "")),
        pts.len()
    );

    // Three hairlines — bottom, middle, top — and their values. Enough to
    // read a magnitude off; more would compete with the one line that matters.
    for t in [0.0, 0.5, 1.0] {
        let v = lo + (hi - lo) * t;
        let y = y_of(v);
        let _ = write!(
            svg,
            "<line class=\"grid\" x1=\"{LEFT}\" x2=\"{:.1}\" y1=\"{y:.1}\" y2=\"{y:.1}\"/>\
             <text class=\"tick\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            WIDTH - RIGHT_PAD,
            LEFT - 8.0,
            y + 4.0,
            esc(&fmt_tick(v))
        );
    }
    let _ = write!(
        svg,
        "<text class=\"label\" x=\"{LEFT}\" y=\"{:.1}\">{}</text>",
        TOP - 10.0,
        esc(label)
    );
    if x_axis && n > 0 {
        // First and last commit only: dates are uneven along a commit axis,
        // so intermediate ticks would imply a time scale that is not there.
        let first = &h.commits[0];
        let last = &h.commits[n - 1];
        let mut ends = vec![(x_of(0, n), "start", first)];
        if n > 1 {
            ends.push((x_of(n - 1, n), "end", last));
        } else {
            ends[0].1 = "middle";
        }
        for (x, anchor, c) in ends {
            let _ = write!(
                svg,
                "<text class=\"tick\" x=\"{x:.1}\" y=\"{:.1}\" text-anchor=\"{anchor}\">{} · {}</text>",
                bottom + 18.0,
                esc(day(&c.date)),
                esc(short(&c.sha))
            );
        }
    }

    if pts.len() > 1 {
        let d: Vec<String> = pts
            .iter()
            .enumerate()
            .map(|(i, (c, v))| {
                format!(
                    "{}{:.1} {:.1}",
                    if i == 0 { "M" } else { "L" },
                    x_of(*c, n),
                    y_of(*v)
                )
            })
            .collect();
        let _ = write!(svg, "<path class=\"line {class}\" d=\"{}\"/>", d.join(" "));
    }
    let spaced = n <= 1 || (WIDTH - LEFT - RIGHT_PAD) / (n - 1) as f64 >= MIN_DOT_SPACING;
    for (k, (c, v)) in pts.iter().enumerate() {
        // Always the latest point, so a long history still shows where it ended.
        if spaced || k + 1 == pts.len() {
            let _ = write!(
                svg,
                "<circle class=\"dot {class}\" cx=\"{:.1}\" cy=\"{:.1}\" r=\"4\"/>",
                x_of(*c, n),
                y_of(*v)
            );
        }
    }

    // Hover targets: a full-height column per point, wider than the mark, so a
    // commit can be found without landing on a four-pixel dot. The native
    // <title> tooltip needs no script, which keeps the page one inert file.
    let deltas = s.deltas();
    let step = if n > 1 {
        (WIDTH - LEFT - RIGHT_PAD) / (n - 1) as f64
    } else {
        WIDTH - LEFT - RIGHT_PAD
    };
    for (p, d) in s.points.iter().zip(deltas) {
        if value(p).is_none() {
            continue;
        }
        let c = &h.commits[p.commit];
        let x = x_of(p.commit, n) - step / 2.0;
        let rect = format!(
            "<rect class=\"hit\" x=\"{:.1}\" y=\"{TOP}\" width=\"{:.1}\" height=\"{:.1}\">\
             <title>{}</title></rect>",
            x.max(LEFT - 8.0),
            step,
            bottom - TOP,
            esc(&tooltip(c, p, d))
        );
        match commit_url(repo, &c.sha) {
            Some(url) => {
                let _ = write!(svg, "<a href=\"{}\">{rect}</a>", esc(&url));
            }
            None => svg.push_str(&rect),
        }
    }
    svg.push_str("</svg>");
    svg
}

/// Theme tokens, light then dark. The dark steps are chosen for the dark
/// surface rather than inverted from the light ones, and wall time is drawn in
/// the neutral text tone in both — it is context, not a second series
/// competing for attention.
const STYLE: &str = r#"
:root {
  color-scheme: light dark;
  --surface: #fcfcfb; --text: #0b0b0b; --muted: #52514e; --grid: #e4e3df;
  --ins: #2a78d6; --wall: #8a8983; --hover: rgba(42, 120, 214, 0.10);
}
@media (prefers-color-scheme: dark) {
  :root {
    --surface: #1a1a19; --text: #ffffff; --muted: #c3c2b7; --grid: #383835;
    --ins: #3987e5; --wall: #8f8e86; --hover: rgba(57, 135, 229, 0.18);
  }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--surface); color: var(--text);
  font: 15px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 780px; margin: 0 auto; padding: 24px 16px 48px; }
h1 { font-size: 1.5rem; margin: 0 0 8px; }
h2 { font-size: 1.1rem; margin: 40px 0 4px; }
h2 .runner { color: var(--muted); font-weight: normal; }
p { margin: 8px 0; }
.muted, .latest { color: var(--muted); }
code { font: 0.9em ui-monospace, SFMono-Regular, Menlo, monospace; }
.charts { overflow-x: auto; }
svg.chart { display: block; width: 100%; min-width: 540px; height: auto; }
.grid { stroke: var(--grid); stroke-width: 1; }
.tick, .label { fill: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }
.line { fill: none; stroke-linejoin: round; stroke-linecap: round; }
.line.ins { stroke: var(--ins); stroke-width: 2; }
.line.wall { stroke: var(--wall); stroke-width: 1.5; }
.dot { stroke: var(--surface); stroke-width: 2; }
.dot.ins { fill: var(--ins); }
.dot.wall { fill: var(--wall); }
.hit { fill: transparent; }
.hit:hover { fill: var(--hover); }
details { margin-top: 8px; }
summary { cursor: pointer; color: var(--muted); }
table { border-collapse: collapse; font-size: 13px; margin-top: 8px; width: 100%; }
th, td { padding: 3px 8px; border-bottom: 1px solid var(--grid); text-align: left;
  vertical-align: top; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
a { color: inherit; }
footer { margin-top: 48px; font-size: 13px; color: var(--muted); }
"#;

/// Render the history as one self-contained HTML page: a chart per series,
/// instruction counts above and wall time beneath it.
///
/// Self-contained — inline SVG and CSS, no script, nothing fetched — so it can
/// be published to GitHub Pages or opened from disk and look the same, and so
/// the page cannot break when a CDN moves.
///
/// Wall time gets its own smaller chart rather than a second axis on the
/// first. Two y-scales on one plot invite reading the crossing of two lines as
/// meaningful, and here one of them is the metric the gate ignores.
pub fn html(h: &History, rev: &str, repo: Option<&str>, credit: bool) -> String {
    let mut out = String::new();
    out.push_str(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Benchmark history</title>\n<style>",
    );
    out.push_str(STYLE);
    out.push_str("</style>\n</head>\n<body>\n<main>\n<h1>Benchmark history</h1>\n");
    let _ = writeln!(
        out,
        "<p class=\"muted\">{}</p>",
        md_code(&esc(&coverage(h, rev)))
    );
    if !h.series.is_empty() {
        let _ = writeln!(
            out,
            "<p class=\"muted\">One chart per benchmark and runner class; measurements \
             from different runner classes are never joined into one line. Each chart is \
             scaled to its own range, never narrower than {:.0}% of its values, so \
             run-to-run noise in instruction counts stays flat. {}</p>",
            INS_MIN_SPAN * 100.0,
            esc(WALL_CAVEAT)
        );
    }

    let header: String = HEADER
        .iter()
        .zip(RIGHT)
        .map(|(h, r)| {
            if r {
                format!("<th class=\"num\">{}</th>", esc(h))
            } else {
                format!("<th>{}</th>", esc(h))
            }
        })
        .collect();

    for s in &h.series {
        let (bench, tool, runner) = &s.key;
        let name = if tool == SELF_TOOL {
            esc(bench)
        } else {
            format!("{} ({})", esc(bench), esc(tool))
        };
        let _ = writeln!(
            out,
            "<section>\n<h2><code>{name}</code> <span class=\"runner\">on <code>{}</code></span></h2>",
            esc(runner)
        );
        let deltas = s.deltas();
        let latest = s
            .points
            .iter()
            .zip(&deltas)
            .rev()
            .find(|(p, _)| p.instructions.is_some());
        match latest {
            Some((p, d)) => {
                let c = &h.commits[p.commit];
                let change = match d {
                    Some(d) => format!(
                        ", {} from the previous measurement",
                        compare::signed_pct(Some(*d))
                    ),
                    None => String::new(),
                };
                let _ = writeln!(
                    out,
                    "<p class=\"latest\">{} instructions at <code>{}</code>{change}</p>",
                    compare::thousands(p.instructions.unwrap_or_default()),
                    esc(short(&c.sha))
                );
            }
            None => out.push_str(
                "<p class=\"latest\">No instruction counts recorded for this series; \
                 timing only.</p>\n",
            ),
        }
        out.push_str("<div class=\"charts\">");
        let has_wall = s.points.iter().any(|p| p.wall_min_ms.is_some());
        if s.points.iter().any(|p| p.instructions.is_some()) {
            out.push_str(&chart(h, s, Metric::Instructions, repo, !has_wall));
        }
        if has_wall {
            out.push_str(&chart(h, s, Metric::Wall, repo, true));
        }
        out.push_str("</div>\n");

        let _ = write!(
            out,
            "<details><summary>{} measurement(s)</summary>\n<table><thead><tr>{header}</tr></thead><tbody>",
            s.points.len()
        );
        for (c, row) in rows(h, s) {
            out.push_str("<tr>");
            for (i, cell) in row.iter().enumerate() {
                let content = match (i, commit_url(repo, &c.sha)) {
                    (0, Some(url)) => {
                        format!("<a href=\"{}\"><code>{}</code></a>", esc(&url), esc(cell))
                    }
                    (0, None) => format!("<code>{}</code>", esc(cell)),
                    _ => esc(cell),
                };
                if RIGHT[i] {
                    let _ = write!(out, "<td class=\"num\">{content}</td>");
                } else {
                    let _ = write!(out, "<td>{content}</td>");
                }
            }
            out.push_str("</tr>");
        }
        out.push_str("</tbody></table></details>\n</section>\n");
    }

    out.push_str("<footer>");
    if credit {
        out.push_str(
            "Generated by <a href=\"https://github.com/jdx/tak\">tak</a>, which counts \
             instructions with Valgrind and stores measurements in this repository's git \
             notes. ",
        );
    }
    out.push_str("tak is pre-v1; this report's format may change between releases.</footer>\n");
    out.push_str("</main>\n</body>\n</html>\n");
    out
}

/// Turn the backtick spans [`coverage`] writes for markdown into `<code>`.
/// Applied after escaping, and backticks are not something escaping touches.
fn md_code(s: &str) -> String {
    let mut out = String::new();
    for (i, part) in s.split('`').enumerate() {
        if i % 2 == 1 {
            let _ = write!(out, "<code>{part}</code>");
        } else {
            out.push_str(part);
        }
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

    fn commit(n: usize, records: Vec<Record>) -> Logged {
        Logged {
            sha: format!("{n:040x}"),
            date: format!("2026-01-{:02}T00:00:00+00:00", n + 1),
            subject: format!("commit {n}"),
            records,
        }
    }

    /// Newest first, as `git log` yields it: commit 4 is the tip.
    fn walk(commits: Vec<Vec<Record>>) -> Vec<Logged> {
        let mut out: Vec<Logged> = commits
            .into_iter()
            .enumerate()
            .map(|(i, r)| commit(i, r))
            .collect();
        out.reverse();
        out
    }

    fn ins(s: &Series) -> Vec<Option<f64>> {
        s.points.iter().map(|p| p.instructions).collect()
    }

    #[test]
    fn a_series_reads_oldest_to_newest() {
        let h = build(
            walk(vec![
                vec![rec("a", "r", 10.0, 1.0)],
                vec![rec("a", "r", 20.0, 1.0)],
                vec![rec("a", "r", 30.0, 1.0)],
            ]),
            10,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(h.series.len(), 1);
        assert_eq!(ins(&h.series[0]), vec![Some(10.0), Some(20.0), Some(30.0)]);
        assert_eq!(h.commits[0].subject, "commit 0");
    }

    /// Most trunk commits carry nothing. They count as walked, and take no
    /// row and no point.
    #[test]
    fn unrecorded_commits_are_walked_not_shown() {
        let h = build(
            walk(vec![
                vec![rec("a", "r", 10.0, 1.0)],
                vec![],
                vec![],
                vec![rec("a", "r", 30.0, 1.0)],
            ]),
            10,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(h.walked, 4);
        assert_eq!(h.commits.len(), 2);
        assert_eq!(h.series[0].deltas(), vec![None, Some(200.0)]);
    }

    /// The same rule as the comparison: a noisy re-run on a commit must not put
    /// a spike in the line that the comparison table does not show.
    #[test]
    fn several_records_on_one_commit_reduce_to_the_minimum() {
        let h = build(
            walk(vec![vec![
                rec("a", "r", 50.0, 9.0),
                rec("a", "r", 10.0, 3.0),
            ]]),
            10,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(h.series[0].points.len(), 1);
        assert_eq!(h.series[0].points[0].instructions, Some(10.0));
        assert_eq!(h.series[0].points[0].wall_min_ms, Some(3.0));
    }

    /// Two runner classes are two series, never one line that jumps where the
    /// machine changed.
    #[test]
    fn runners_are_never_mixed() {
        let h = build(
            walk(vec![
                vec![rec("a", "old", 10.0, 1.0)],
                vec![rec("a", "new", 99.0, 1.0)],
            ]),
            10,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(h.series.len(), 2);
        for s in &h.series {
            assert_eq!(s.points.len(), 1, "{:?}", s.key);
            assert_eq!(s.deltas(), vec![None], "no delta across runners");
        }
    }

    /// A benchmark added partway through starts partway through, and its first
    /// point has nothing to be a change from.
    #[test]
    fn a_series_can_appear_and_disappear() {
        let h = build(
            walk(vec![
                vec![rec("a", "r", 1.0, 1.0)],
                vec![rec("a", "r", 1.0, 1.0), rec("b", "r", 5.0, 1.0)],
                vec![rec("b", "r", 6.0, 1.0)],
            ]),
            10,
            &[],
            false,
        )
        .unwrap();
        let a = &h.series[0];
        let b = &h.series[1];
        assert_eq!(
            a.points.iter().map(|p| p.commit).collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            b.points.iter().map(|p| p.commit).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(b.deltas(), vec![None, Some(20.0)]);
    }

    /// The limit counts recorded commits, newest first, and says that it left
    /// older ones out.
    #[test]
    fn the_limit_keeps_the_newest_and_counts_the_rest() {
        let h = build(
            walk((0..5).map(|i| vec![rec("a", "r", i as f64, 1.0)]).collect()),
            2,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(ins(&h.series[0]), vec![Some(3.0), Some(4.0)]);
        assert!(h.older);
        let md = markdown(&h, "HEAD", false);
        assert!(md.contains("Older recorded commits are not shown"), "{md}");
    }

    /// With a filter, a commit carrying only other benchmarks is not a
    /// recorded commit — or `-n 5 --bench x` shows fewer than five points.
    #[test]
    fn the_limit_counts_commits_of_the_selected_bench() {
        let h = build(
            walk(vec![
                vec![rec("a", "r", 1.0, 1.0)],
                vec![rec("b", "r", 1.0, 1.0)],
                vec![rec("a", "r", 2.0, 1.0)],
                vec![rec("b", "r", 1.0, 1.0)],
            ]),
            2,
            &["a".to_string()],
            false,
        )
        .unwrap();
        assert_eq!(h.series.len(), 1);
        assert_eq!(ins(&h.series[0]), vec![Some(1.0), Some(2.0)]);
        assert!(!h.older);
    }

    /// A typo in a CI step must fail, not publish an empty report forever.
    #[test]
    fn an_unknown_bench_is_an_error_that_names_what_exists() {
        let err = build(
            walk(vec![vec![rec("startup", "r", 1.0, 1.0)]]),
            10,
            &["statrup".to_string()],
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`statrup`"), "{err}");
        assert!(err.contains("startup"), "{err}");
    }

    /// A shallow clone that ran out of commits is not the same as a project
    /// that ran out of history, and the report must not read as though it were.
    #[test]
    fn a_shallow_walk_says_so() {
        let h = build(walk(vec![vec![rec("a", "r", 1.0, 1.0)]]), 10, &[], true).unwrap();
        assert!(markdown(&h, "HEAD", false).contains("shallow"));
        assert!(html(&h, "HEAD", None, false).contains("shallow"));
        // When the limit stopped the walk, shallowness is beside the point.
        let h = build(
            walk(vec![
                vec![rec("a", "r", 1.0, 1.0)],
                vec![rec("a", "r", 1.0, 1.0)],
            ]),
            1,
            &[],
            true,
        )
        .unwrap();
        assert!(!markdown(&h, "HEAD", false).contains("shallow"));
    }

    #[test]
    fn nothing_recorded_says_so() {
        let h = build(walk(vec![vec![], vec![]]), 10, &[], false).unwrap();
        let md = markdown(&h, "main", false);
        assert!(md.contains("No measurements recorded"), "{md}");
        assert!(md.contains("2 commit(s) walked"), "{md}");
        // An empty page still has to be a valid page.
        assert!(html(&h, "main", None, false).contains("</html>"));
    }

    #[test]
    fn markdown_rows_are_newest_first_with_deltas() {
        let h = build(
            walk(vec![
                vec![rec("a", "r", 1_000_000.0, 10.0)],
                vec![rec("a", "r", 1_020_000.0, 11.0)],
            ]),
            10,
            &[],
            false,
        )
        .unwrap();
        let md = markdown(&h, "HEAD", false);
        let newest = md.find("1,020,000").unwrap();
        let oldest = md.find("1,000,000").unwrap();
        assert!(newest < oldest, "{md}");
        assert!(md.contains("+2.00%"), "{md}");
        assert!(md.contains("### `a` on `r`"), "{md}");
        assert!(md.contains("Only instruction counts gate"), "{md}");
        // Every column but the ragged last one lines up unrendered.
        let starts: BTreeSet<usize> = md
            .lines()
            .filter(|l| l.starts_with('|'))
            .map(|l| {
                l.chars()
                    .enumerate()
                    .filter(|(_, c)| *c == '|')
                    .nth(5)
                    .unwrap()
                    .0
            })
            .collect();
        assert_eq!(starts.len(), 1, "{starts:?}\n{md}");
    }

    /// A subject that escapes its own pipe must not end up with the escape
    /// escaped and the pipe bare.
    #[test]
    fn backslashes_in_subjects_are_escaped_first() {
        let mut w = walk(vec![vec![rec("a", "r", 1.0, 1.0)]]);
        w[0].subject = r"fix(a\|b)".into();
        let h = build(w, 10, &[], false).unwrap();
        let md = markdown(&h, "HEAD", false);
        assert!(md.contains(r"fix(a\\\|b)"), "{md}");
        // Every pipe not preceded by an odd run of backslashes is a cell
        // boundary, and six cells have seven.
        let row = md.lines().find(|l| l.contains("fix(")).unwrap();
        let bare = row
            .char_indices()
            .filter(|&(i, c)| {
                c == '|' && row[..i].chars().rev().take_while(|&b| b == '\\').count() % 2 == 0
            })
            .count();
        assert_eq!(bare, 7, "{row}");
    }

    /// Records carrying only metrics this report does not draw must not use up
    /// `-n` slots that drawable measurements then lose.
    #[test]
    fn undrawable_records_do_not_count_toward_the_limit() {
        let mut custom = rec("a", "r", 0.0, 0.0);
        custom.metrics = BTreeMap::from([("binary_bytes".to_string(), 9.0)]);
        let h = build(
            walk(vec![
                vec![rec("a", "r", 1.0, 1.0)],
                vec![rec("a", "r", 2.0, 1.0)],
                vec![custom.clone()],
                vec![custom],
            ]),
            2,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(h.commits.len(), 2);
        assert_eq!(ins(&h.series[0]), vec![Some(1.0), Some(2.0)]);
        assert!(!h.older);
    }

    /// A selected benchmark with nothing drawable must fail, not publish a
    /// blank report — and say why, rather than claiming it was never recorded.
    #[test]
    fn a_selected_bench_with_nothing_drawable_is_an_error() {
        let mut custom = rec("size", "r", 0.0, 0.0);
        custom.metrics = BTreeMap::from([("binary_bytes".to_string(), 9.0)]);
        let err = build(
            walk(vec![vec![rec("a", "r", 1.0, 1.0), custom]]),
            10,
            &["size".to_string()],
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`size` has no instructions"), "{err}");
        assert!(err.contains("only other metrics"), "{err}");
    }

    /// Without a filter an empty history is a legitimate state, but one made
    /// of undrawable records must not read as though nothing was recorded.
    #[test]
    fn an_all_undrawable_history_says_what_it_skipped() {
        let mut custom = rec("size", "r", 0.0, 0.0);
        custom.metrics = BTreeMap::from([("binary_bytes".to_string(), 9.0)]);
        let h = build(walk(vec![vec![custom], vec![]]), 10, &[], false).unwrap();
        assert!(h.series.is_empty());
        let md = markdown(&h, "HEAD", false);
        assert!(md.contains("1 carried only other metrics"), "{md}");
        assert!(!md.contains("No measurements recorded"), "{md}");
    }

    /// A pipe in a commit subject would otherwise end its table row early.
    #[test]
    fn subjects_cannot_break_the_table() {
        let mut w = walk(vec![vec![rec("a", "r", 1.0, 1.0)]]);
        w[0].subject = "fix(a|b): <thing>".into();
        let h = build(w, 10, &[], false).unwrap();
        assert!(markdown(&h, "HEAD", false).contains("fix(a\\|b)"));
        let page = html(&h, "HEAD", None, false);
        assert!(page.contains("fix(a|b): &lt;thing&gt;"), "{page}");
        assert!(!page.contains("<thing>"));
    }

    #[test]
    fn the_credit_line_follows_the_setting() {
        let h = build(walk(vec![vec![rec("a", "r", 1.0, 1.0)]]), 10, &[], false).unwrap();
        assert!(markdown(&h, "HEAD", true).contains("Measured by [tak]"));
        assert!(!markdown(&h, "HEAD", false).contains("Measured by [tak]"));
        assert!(html(&h, "HEAD", None, true).contains("Generated by"));
        assert!(!html(&h, "HEAD", None, false).contains("Generated by"));
    }

    #[test]
    fn the_page_is_self_contained() {
        let h = build(
            walk(vec![
                vec![rec("a", "r", 1.0, 1.0)],
                vec![rec("a", "r", 2.0, 1.0)],
            ]),
            10,
            &[],
            false,
        )
        .unwrap();
        let page = html(&h, "HEAD", Some("o/r"), true);
        assert!(!page.contains("<script"), "no script");
        assert!(!page.contains("<link"), "no external stylesheet");
        assert!(!page.contains("src="), "nothing loaded");
        assert!(page.contains("prefers-color-scheme: dark"));
        // One chart for instructions, one for wall time.
        assert_eq!(page.matches("<svg").count(), 2, "{page}");
        assert!(page.contains("<title>"), "hover titles");
    }

    #[test]
    fn commits_link_to_the_forge_only_when_there_is_one() {
        let h = build(walk(vec![vec![rec("a", "r", 1.0, 1.0)]]), 10, &[], false).unwrap();
        let sha = &h.commits[0].sha;
        let linked = html(&h, "HEAD", Some("jdx/tak"), false);
        assert!(
            linked.contains(&format!("https://github.com/jdx/tak/commit/{sha}")),
            "{linked}"
        );
        assert!(!html(&h, "HEAD", None, false).contains("https://github.com/jdx/tak/commit"));
    }

    /// Wall-only series — a competitor tool — still get a chart, and say
    /// they have no gate-able number rather than drawing an empty one.
    #[test]
    fn a_timing_only_series_draws_only_timing() {
        let mut r = rec("install", "r", 0.0, 12.0);
        r.tool = "pnpm".into();
        r.metrics.remove(GATED_METRIC);
        let h = build(walk(vec![vec![r]]), 10, &[], false).unwrap();
        let page = html(&h, "HEAD", None, false);
        assert!(page.contains("timing only"), "{page}");
        assert_eq!(page.matches("<svg").count(), 1);
        assert!(page.contains("install (pnpm)"), "{page}");
    }

    /// A deterministic metric wobbling by 0.02% must not fill the chart. The
    /// floor on the span keeps it near the middle.
    #[test]
    fn noise_does_not_fill_the_chart() {
        let (lo, hi) = domain(&[29_413_235.0, 29_419_117.0], INS_MIN_SPAN);
        assert!(hi - lo >= 29_413_235.0 * INS_MIN_SPAN, "{lo} {hi}");
        // A step the size of a real regression is not clipped.
        let (lo, hi) = domain(&[100.0, 150.0], INS_MIN_SPAN);
        assert!(lo < 100.0 && hi > 150.0);
    }

    /// Flat is the best outcome and sits mid-height, not at the floor.
    #[test]
    fn a_flat_series_is_centred() {
        let (lo, hi) = domain(&[500.0, 500.0], INS_MIN_SPAN);
        assert!(((lo + hi) / 2.0 - 500.0).abs() < 1e-9);
        // All zeros is a range, not a division by zero.
        let (lo, hi) = domain(&[0.0], INS_MIN_SPAN);
        assert!(hi > lo && lo >= 0.0);
    }

    #[test]
    fn si_keeps_enough_figures_to_tell_ticks_apart() {
        assert_eq!(si(29_413_235.0), "29.41M");
        assert_eq!(si(1_500.0), "1.500k");
        assert_eq!(si(2_300_000_000.0), "2.300G");
        assert_eq!(si(12.0), "12.00");
        // The narrowest chart spans 2%: its top and bottom labels differ.
        let (lo, hi) = domain(&[29_413_235.0], INS_MIN_SPAN);
        assert_ne!(si(lo), si(hi));
    }

    #[test]
    fn a_zero_base_has_no_percentage() {
        let h = build(
            walk(vec![
                vec![rec("a", "r", 0.0, 1.0)],
                vec![rec("a", "r", 5.0, 1.0)],
            ]),
            10,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(h.series[0].deltas(), vec![None, None]);
    }
}
