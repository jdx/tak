//! Per-function instruction profiles, and where a change between two of them
//! went.
//!
//! A failed gate says "+3.2% instructions". That is enough to stop a merge and
//! not enough to fix anything. cachegrind already knows which functions
//! retired those instructions — the counting run discarded it to `/dev/null`
//! — so `tak run --profile-dir` keeps the profile of the run whose count was
//! reported, and `tak explain` ranks the functions whose counts moved. Timing
//! tools cannot do this: a sampled profile of a 10ms command is mostly noise,
//! while these counts are as deterministic as the total they add up to.
//!
//! The cachegrind out format is parsed here rather than handed to `cg_diff`
//! or `cg_annotate`. Both are Python scripts in recent valgrind releases, and
//! a CI image that installs valgrind for counting need not carry Python.
//!
//! Profiles never go into git notes. A profile is hundreds of kilobytes, and a
//! note is a set of one-line records that concurrent writers deduplicate by
//! exact bytes — see [`crate::record`].

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a kept profile's file name ends in: `<bench>/<subject>` plus this.
/// cachegrind's own default is `cachegrind.out.<pid>`; the pid is useless
/// here and would stop a base and a head from pairing up by name.
pub const EXTENSION: &str = ".cachegrind.out";

/// Where a profile came from, written into it as `desc: tak <key>: <value>`
/// lines.
///
/// `desc:` is the format's own slot for free-form description, and
/// `cg_annotate` prints it, so a profile that has been copied out of its
/// directory still says where it came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Origin {
    /// The runner class. It matters for the same reason the notes are
    /// partitioned on it: between runner classes a different glibc picks a
    /// different `memcpy`, and an attribution across them reads as one
    /// function vanishing and another appearing.
    pub runner: String,
    /// The commit measured, when there was one. With the benchmark and
    /// subject, it lets `tak explain` find what the notes recorded for the
    /// same series and say when a later run replaced the profile behind it.
    pub commit: Option<String>,
    pub bench: String,
    pub subject: String,
}

/// Where `tak run --profile-dir DIR` keeps a subject's profile:
/// `DIR/<bench>/<subject>.cachegrind.out`.
///
/// Benchmark and subject names come from `tak.toml` keys, which may be any
/// string. A name that is not a single, ordinary path component is refused
/// rather than escaped: `[bench."../x"]` writing outside the directory is
/// the failure being prevented, and an escaped name would no longer be the
/// name a reader looks for. Control characters are refused too: a newline
/// would split the `desc:` line the name is written into, and ends up in a
/// report heading.
pub fn path_for(dir: &Path, bench: &str, subject: &str) -> Result<PathBuf> {
    for (what, name) in [("benchmark", bench), ("subject", subject)] {
        let plain = !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains(['/', '\\'])
            && !name.chars().any(char::is_control);
        if !plain {
            bail!(
                "--profile-dir cannot name a file after {what} {name:?}: it is not a plain file name"
            );
        }
    }
    Ok(dir.join(bench).join(format!("{subject}{EXTENSION}")))
}

/// Write a profile to `dest`, with where it came from.
///
/// Through a temporary file in the same directory and a rename, so an
/// interrupted run leaves the previous profile or the new one, never half of
/// one that parses as a function set with most of its functions missing.
pub fn write(dest: &Path, raw: &[u8], origin: &Origin) -> Result<()> {
    let parent = dest.parent().context("a profile path has a directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("could not create {}", parent.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("could not write a profile in {}", parent.display()))?;
    // `desc:` lines lead the file in the format's grammar, so prepending them
    // leaves it readable by cachegrind's own tools. One line each, whatever
    // a value holds.
    let mut head = String::new();
    let fields = [
        ("runner", Some(origin.runner.as_str())),
        ("commit", origin.commit.as_deref()),
        ("bench", Some(origin.bench.as_str())),
        ("subject", Some(origin.subject.as_str())),
    ];
    for (key, value) in fields {
        if let Some(v) = value {
            let v: String = v
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            head.push_str(&format!("desc: tak {key}: {v}\n"));
        }
    }
    use std::io::Write;
    tmp.write_all(head.as_bytes())
        .and_then(|()| tmp.write_all(raw))
        .with_context(|| format!("could not write {}", dest.display()))?;
    tmp.persist(dest)
        .with_context(|| format!("could not write {}", dest.display()))?;
    Ok(())
}

/// One function's self cost, and the source files it was charged under.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Function {
    /// Instructions retired in the function's own code.
    pub ir: u64,
    /// The same, by source file. cachegrind charges a line to the file it
    /// came from, so a function with a helper inlined into it shows up under
    /// the helper's header as well as its own source.
    pub files: BTreeMap<String, u64>,
}

impl Function {
    /// The file most of the function's instructions came from — the one a
    /// reader would open. Ties go to the first name, so the choice is stable.
    pub fn file(&self) -> &str {
        self.files
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map_or("???", |(f, _)| f.as_str())
    }
}

/// A parsed cachegrind out file, reduced to instructions per function.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// `desc:` lines, in order.
    pub desc: Vec<String>,
    /// The `cmd:` line: what was profiled.
    pub cmd: Option<String>,
    /// The `summary:` total, or the sum of the functions when there is none.
    pub total: u64,
    /// Keyed on the function's name alone, not on file and name as
    /// cachegrind does. Its file is wherever the line came from, and two
    /// builds in different checkouts, or with a helper inlined differently,
    /// would otherwise disagree about every function in the program.
    pub functions: BTreeMap<String, Function>,
}

impl Profile {
    /// What `tak run --profile-dir` recorded about where this came from, when
    /// it wrote the file. `None` for a profile from anywhere else.
    pub fn origin(&self) -> Option<Origin> {
        let get = |key: &str| {
            self.desc.iter().find_map(|d| {
                d.strip_prefix("tak ")?
                    .strip_prefix(key)?
                    .strip_prefix(": ")
                    .map(str::to_string)
            })
        };
        Some(Origin {
            runner: get("runner")?,
            commit: get("commit"),
            bench: get("bench").unwrap_or_default(),
            subject: get("subject").unwrap_or_default(),
        })
    }

    /// The runner class `tak run --profile-dir` recorded, if it wrote this.
    pub fn runner(&self) -> Option<String> {
        self.origin().map(|o| o.runner)
    }

    pub fn load(path: &Path) -> Result<Profile> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        parse(&text).with_context(|| format!("could not parse {}", path.display()))
    }
}

/// The two name tables of the format's compression: `fl=(3) src/a.rs` defines
/// 3, and a later `fl=(3)` refers back to it. Files and functions are numbered
/// separately.
#[derive(Default)]
struct Names {
    files: BTreeMap<String, String>,
    fns: BTreeMap<String, String>,
}

/// Resolve a possibly-compressed name.
///
/// cachegrind itself writes names in full; callgrind compresses them, and a
/// profile converted by other tooling may too. `(id)` alone refers to an
/// earlier definition; `(id) name` defines one.
fn resolve(table: &mut BTreeMap<String, String>, value: &str) -> Result<String> {
    let value = value.trim();
    let Some(rest) = value.strip_prefix('(') else {
        return Ok(value.to_string());
    };
    let Some((id, name)) = rest.split_once(')') else {
        // `(below main)` is a real function name, not a malformed reference.
        return Ok(value.to_string());
    };
    if !id.chars().all(|c| c.is_ascii_digit()) || id.is_empty() {
        return Ok(value.to_string());
    }
    let name = name.trim();
    if name.is_empty() {
        return table
            .get(id)
            .cloned()
            .with_context(|| format!("name ({id}) is used before it is defined"));
    }
    table.insert(id.to_string(), name.to_string());
    Ok(name.to_string())
}

/// Parse a cachegrind out file.
///
/// Only instructions (`Ir`) are kept; tak runs cachegrind with the cache and
/// branch simulations off, and those are the only events a count gates on.
///
/// Tolerant of what it does not need — unknown header and specification
/// lines are skipped — and strict about what it does: a cost line that will
/// not parse is an error with its line number, because silently dropping one
/// understates a function and the ranking built on it would be wrong.
pub fn parse(text: &str) -> Result<Profile> {
    let mut p = Profile::default();
    let mut names = Names::default();
    // Which cost column is `Ir`, and how many position columns precede the
    // costs: cachegrind writes one (the line), callgrind may write more.
    let mut ir_col: Option<usize> = None;
    let mut positions = 1usize;
    let mut summary: Option<u64> = None;
    let mut file = String::from("???");
    let mut func = String::from("???");
    // callgrind's `calls=` is followed by the call's *inclusive* cost, which
    // is already counted in the callee's own lines.
    let mut skip_cost = false;
    let mut sum = 0u64;

    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        // Both ends: cachegrind writes no indentation, but a converted or
        // hand-edited profile may, and an indented cost line skipped as
        // unrecognised would understate its function while the total stood.
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let first = line.as_bytes()[0];
        if first.is_ascii_digit() || matches!(first, b'+' | b'-' | b'*') {
            if skip_cost {
                skip_cost = false;
                continue;
            }
            let col =
                ir_col.with_context(|| format!("line {n}: a cost before the `events:` line"))?;
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < positions {
                bail!("line {n}: expected {positions} position column(s): {line}");
            }
            // A trailing zero cost may be left off, so a short line is zero.
            let ir = match fields.get(positions + col) {
                Some(v) => v
                    .parse::<u64>()
                    .with_context(|| format!("line {n}: not a count: {v}"))?,
                None => 0,
            };
            let f = p.functions.entry(func.clone()).or_default();
            f.ir += ir;
            *f.files.entry(file.clone()).or_default() += ir;
            sum += ir;
            continue;
        }
        if let Some(rest) = line.strip_prefix("desc:") {
            p.desc.push(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("cmd:") {
            p.cmd = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("events:") {
            let events: Vec<&str> = rest.split_whitespace().collect();
            ir_col = Some(
                events
                    .iter()
                    .position(|e| *e == "Ir")
                    .with_context(|| format!("line {n}: no `Ir` event in `{line}`"))?,
            );
        } else if let Some(rest) = line.strip_prefix("positions:") {
            positions = rest.split_whitespace().count().max(1);
        } else if let Some(rest) = line
            .strip_prefix("summary:")
            .or_else(|| line.strip_prefix("totals:"))
        {
            let col = ir_col.with_context(|| format!("line {n}: a total before `events:`"))?;
            if let Some(v) = rest.split_whitespace().nth(col) {
                summary = Some(
                    v.parse()
                        .with_context(|| format!("line {n}: not a count: {v}"))?,
                );
            }
        } else if let Some((key, value)) = line.split_once('=') {
            match key {
                // `fi=`/`fe=` switch file for inlined code within the same
                // function; `fl=` starts a new function's file.
                "fl" | "fi" | "fe" => file = resolve(&mut names.files, value)?,
                "fn" => func = resolve(&mut names.fns, value)?,
                // Callee names still define compressed ids that a later
                // `fn=(id)` may refer to.
                "cfl" | "cfi" => {
                    resolve(&mut names.files, value)?;
                }
                "cfn" => {
                    resolve(&mut names.fns, value)?;
                }
                "calls" => skip_cost = true,
                _ => {}
            }
        }
        // Anything else — `version:`, `creator:`, `pid:` and the like — says
        // nothing about where instructions went.
    }
    if ir_col.is_none() {
        bail!("not a cachegrind profile: no `events:` line");
    }
    p.total = summary.unwrap_or(sum);
    Ok(p)
}

/// One function's count on both sides.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub name: String,
    /// Where it lives, from the head when it exists there.
    pub file: String,
    pub base: u64,
    pub head: u64,
}

impl Delta {
    pub fn change(&self) -> i128 {
        self.head as i128 - self.base as i128
    }
}

/// Every function whose count differs, largest change first.
///
/// By size of the change in either direction, not by increase alone. A
/// refactor that moves work shows as one function gaining and another losing,
/// and a reader seeing only the gain would go looking for new work that is
/// not there.
pub fn diff(base: &Profile, head: &Profile) -> Vec<Delta> {
    let empty = Function::default();
    let mut out: Vec<Delta> = base
        .functions
        .keys()
        .chain(head.functions.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| {
            let b = base.functions.get(name).unwrap_or(&empty);
            let h = head.functions.get(name).unwrap_or(&empty);
            (b.ir != h.ir).then(|| Delta {
                name: name.clone(),
                file: if h.files.is_empty() { b } else { h }.file().to_string(),
                base: b.ir,
                head: h.ir,
            })
        })
        .collect();
    // Name breaks ties, so the same two profiles always render the same table.
    out.sort_by(|a, b| {
        b.change()
            .unsigned_abs()
            .cmp(&a.change().unsigned_abs())
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// A base and a head profile of the same benchmark subject.
#[derive(Debug, Clone)]
pub struct Pair {
    /// How the report names it: `bench`, or `bench (subject)`.
    pub label: String,
    pub base: Profile,
    pub head: Profile,
}

/// Profiles found on only one side, by their path relative to its directory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Unpaired {
    pub base: Vec<String>,
    pub head: Vec<String>,
}

/// The profiles `tak run --profile-dir` wrote under `dir`, by relative path.
fn scan(dir: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    let benches =
        std::fs::read_dir(dir).with_context(|| format!("could not read {}", dir.display()))?;
    for bench in benches {
        let bench = bench?;
        if !bench.file_type()?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(bench.path())? {
            let file = file?;
            let name = file.file_name().to_string_lossy().to_string();
            if file.file_type()?.is_file() && name.ends_with(EXTENSION) {
                out.insert(
                    format!("{}/{name}", bench.file_name().to_string_lossy()),
                    file.path(),
                );
            }
        }
    }
    Ok(out)
}

/// `bench/subject.cachegrind.out` as the comparison table names it, as
/// markdown.
///
/// Escaped, because names come from `tak.toml` and file names, and in CI the
/// pull request being reported on controls both. A name must stay a label:
/// one that could open a heading or a link could write its own claims into
/// the step summary.
fn label(rel: &str) -> String {
    let (bench, file) = rel.split_once('/').unwrap_or(("", rel));
    let subject = file.strip_suffix(EXTENSION).unwrap_or(file);
    match bench {
        "" => text(subject),
        _ if subject == crate::config::SELF_TOOL => text(bench),
        _ => format!("{} ({})", text(bench), text(subject)),
    }
}

/// Untrusted text as inert markdown: control characters, newlines among
/// them, become spaces, and anything markdown could read as syntax is
/// backslash-escaped.
fn text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            out.push(' ');
            continue;
        }
        if "\\`*_[]<>#|!~&".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Pair up what to explain: two directories `tak run --profile-dir` wrote,
/// matched on `<bench>/<subject>`, or two profile files.
pub fn load(base: &Path, head: &Path) -> Result<(Vec<Pair>, Unpaired)> {
    for p in [base, head] {
        if !p.exists() {
            bail!("{} does not exist", p.display());
        }
    }
    match (base.is_dir(), head.is_dir()) {
        (false, false) => {
            let name = head
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let pair = Pair {
                label: label(&name),
                base: Profile::load(base)?,
                head: Profile::load(head)?,
            };
            Ok((vec![pair], Unpaired::default()))
        }
        (true, true) => {
            let (b, h) = (scan(base)?, scan(head)?);
            let mut pairs = Vec::new();
            let mut unpaired = Unpaired::default();
            for (rel, path) in &h {
                match b.get(rel) {
                    Some(bpath) => pairs.push(Pair {
                        label: label(rel),
                        base: Profile::load(bpath)?,
                        head: Profile::load(path)?,
                    }),
                    None => unpaired.head.push(rel.clone()),
                }
            }
            unpaired.base = b.keys().filter(|k| !h.contains_key(*k)).cloned().collect();
            Ok((pairs, unpaired))
        }
        _ => bail!(
            "compare a directory with a directory, or a profile with a profile: {} and {}",
            base.display(),
            head.display()
        ),
    }
}

/// A name as a code span, fit for a table cell. Rust and C++ names carry `|`
/// in closures and `<`…`>` in generics; the pipe would end the cell, a
/// backtick would end the code span, and a newline would end the row.
fn cell(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| match c {
            '`' => '\'',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    format!("`{}`", name.replace('|', "\\|"))
}

/// A warning when a profile is not from the run whose count the notes hold.
///
/// Profiles are overwritten by the next run, while `tak compare` takes the
/// lowest of every count recorded for a commit. Measure twice — a rebuilt
/// binary on the same commit, or a subject whose work varies — and the
/// profile left behind can describe a different count from the one the gate
/// used, which is worth saying rather than explaining the wrong number.
fn mismatch(side: &str, p: &Profile, recorded: &dyn Fn(&Origin) -> Option<u64>) -> String {
    let Some(origin) = p.origin() else {
        return String::new();
    };
    match (origin.commit.as_deref(), recorded(&origin)) {
        (Some(commit), Some(n)) if n != p.total => format!(
            "\n> [!WARNING]\n> The {side} profile totals {} instructions, but the lowest count \
             recorded for {} in git notes is {}. The profile is not from the run that count \
             came from.\n",
            crate::compare::thousands(p.total as f64),
            cell(&commit[..commit.len().min(12)]),
            crate::compare::thousands(n as f64),
        ),
        _ => String::new(),
    }
}

fn signed(v: i128) -> String {
    let n = crate::compare::thousands(v.unsigned_abs() as f64);
    match v.signum() {
        1 => format!("+{n}"),
        -1 => format!("-{n}"),
        _ => n,
    }
}

/// Render the attribution as markdown, for a pull request comment or a step
/// summary beneath `tak compare`'s table.
///
/// The rows shown and a final row for the rest add up to the total change,
/// so a reader can see how much of it the table accounts for.
///
/// `recorded` looks up the instruction count git notes hold for a profile's
/// series, so a profile replaced since that count was recorded is called out.
/// It is a parameter so this stays free of git.
pub fn markdown(
    pairs: &[Pair],
    unpaired: &Unpaired,
    top: usize,
    recorded: &dyn Fn(&Origin) -> Option<u64>,
) -> String {
    let mut out = String::from("## Where the instructions went\n");
    if pairs.is_empty() {
        out.push_str("\nNo benchmark has a profile on both sides, so nothing was explained.\n");
    }
    for pair in pairs {
        let (base, head) = (pair.base.total, pair.head.total);
        let change = head as i128 - base as i128;
        let pct = if base == 0 {
            String::new()
        } else {
            format!(
                ", **{}{:.2}%**",
                if change >= 0 { "+" } else { "" },
                change as f64 / base as f64 * 100.0
            )
        };
        out.push_str(&format!(
            "\n### {}\n\n{} → {} instructions ({}{pct}).\n",
            pair.label,
            crate::compare::thousands(base as f64),
            crate::compare::thousands(head as f64),
            signed(change),
        ));
        if let (Some(b), Some(h)) = (pair.base.runner(), pair.head.runner())
            && b != h
        {
            out.push_str(&format!(
                "\n> [!WARNING]\n> Profiled on different runner classes ({} and {}). \
                 Library code differs between machines, so some of what follows is the \
                 machine rather than the change.\n",
                cell(&b),
                cell(&h)
            ));
        }
        out.push_str(&mismatch("base", &pair.base, recorded));
        out.push_str(&mismatch("head", &pair.head, recorded));
        let deltas = diff(&pair.base, &pair.head);
        if deltas.is_empty() {
            out.push_str("\nNo function's instruction count changed.\n");
            continue;
        }
        let shown = deltas.len().min(top);
        if shown < deltas.len() {
            out.push_str(&format!(
                "\n{} function(s) changed; the {shown} largest:\n\n",
                deltas.len()
            ));
        } else {
            out.push_str(&format!("\n{} function(s) changed:\n\n", deltas.len()));
        }
        out.push_str("| function | Δ | base | head | file |\n|---|---:|---:|---:|---|\n");
        for d in &deltas[..shown] {
            out.push_str(&format!(
                "| {} | **{}** | {} | {} | {} |\n",
                cell(&d.name),
                signed(d.change()),
                crate::compare::thousands(d.base as f64),
                crate::compare::thousands(d.head as f64),
                cell(&d.file),
            ));
        }
        let rest = &deltas[shown..];
        if !rest.is_empty() {
            let sum: i128 = rest.iter().map(Delta::change).sum();
            out.push_str(&format!(
                "| {} more | {} | | | |\n",
                rest.len(),
                signed(sum)
            ));
        }
    }
    for (side, list) in [("base", &unpaired.base), ("head", &unpaired.head)] {
        if !list.is_empty() {
            out.push_str(&format!(
                "\nOnly in the {side}, so not explained: {}\n",
                list.iter().map(|p| cell(p)).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    out.push_str(
        "\n<sub>Self cost per function: code inlined into a function counts as its own. \
         Only the benchmarked process is profiled, not programs it starts. \
         `???` is code without symbols.</sub>\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = include_str!("../tests/fixtures/profile/base.cachegrind.out");
    const HEAD: &str = include_str!("../tests/fixtures/profile/head.cachegrind.out");

    #[test]
    fn a_profile_sums_each_function_across_its_files() {
        let p = parse(BASE).unwrap();
        assert_eq!(p.cmd.as_deref(), Some("./mycli --help"));
        assert_eq!(p.total, 1_500);
        // `parse_args` has a helper inlined from another file; both count.
        let f = &p.functions["mycli::parse_args"];
        assert_eq!(f.ir, 600);
        assert_eq!(f.files["src/args.rs"], 500);
        assert_eq!(f.files["src/util.rs"], 100);
        assert_eq!(f.file(), "src/args.rs");
        assert_eq!(p.functions.values().map(|f| f.ir).sum::<u64>(), p.total);
    }

    #[test]
    fn the_largest_change_comes_first_in_either_direction() {
        let d = diff(&parse(BASE).unwrap(), &parse(HEAD).unwrap());
        let got: Vec<(&str, i128)> = d.iter().map(|d| (d.name.as_str(), d.change())).collect();
        assert_eq!(
            got,
            [
                ("mycli::render", 900),
                ("mycli::parse_args", -300),
                ("mycli::new_helper", 50),
                ("mycli::gone", -40),
            ]
        );
        // A function only on one side is a change from or to zero.
        let new = d.iter().find(|d| d.name == "mycli::new_helper").unwrap();
        assert_eq!((new.base, new.head), (0, 50));
        assert_eq!(new.file, "src/new.rs");
        let gone = d.iter().find(|d| d.name == "mycli::gone").unwrap();
        assert_eq!(gone.file, "src/old.rs");
    }

    #[test]
    fn identical_profiles_have_no_changes() {
        let p = parse(BASE).unwrap();
        assert!(diff(&p, &p).is_empty());
    }

    #[test]
    fn a_missing_summary_is_the_sum() {
        let p = parse("events: Ir\nfl=a.c\nfn=f\n1 5\n2 7\n").unwrap();
        assert_eq!(p.total, 12);
    }

    #[test]
    fn compressed_names_resolve_to_their_definition() {
        let text = "\
events: Ir
fl=(1) src/a.c
fn=(1) alpha
1 10
fn=(2) beta
2 20
fl=(1)
fn=(1)
3 5
";
        let p = parse(text).unwrap();
        assert_eq!(p.functions["alpha"].ir, 15);
        assert_eq!(p.functions["beta"].ir, 20);
        assert_eq!(p.functions["alpha"].files["src/a.c"], 15);
    }

    /// `(below main)` is how valgrind names the code before `main`, and must
    /// not be mistaken for a compressed reference.
    #[test]
    fn a_parenthesised_name_is_a_name() {
        let p = parse("events: Ir\nfl=???\nfn=(below main)\n0 9\n").unwrap();
        assert_eq!(p.functions["(below main)"].ir, 9);
    }

    #[test]
    fn a_reference_to_an_undefined_name_is_an_error() {
        let err = parse("events: Ir\nfn=(7)\n1 1\n").unwrap_err();
        assert!(format!("{err:#}").contains("(7)"), "{err:#}");
    }

    /// callgrind's call cost is inclusive and already counted in the callee,
    /// so adding it would count those instructions twice.
    #[test]
    fn the_cost_after_a_call_is_not_self_cost() {
        let text = "\
events: Ir
fl=a.c
fn=caller
1 3
cfn=callee
calls=1 10
1 100
2 4
fn=callee
10 100
";
        let p = parse(text).unwrap();
        assert_eq!(p.functions["caller"].ir, 7);
        assert_eq!(p.functions["callee"].ir, 100);
    }

    #[test]
    fn ir_is_found_among_other_events() {
        let text = "events: Dr Ir Dw\nfl=a.c\nfn=f\n1 100 5 100\n2 100 6\nsummary: 999 11 999\n";
        let p = parse(text).unwrap();
        assert_eq!(p.functions["f"].ir, 11);
        assert_eq!(p.total, 11);
    }

    #[test]
    fn a_malformed_cost_names_its_line() {
        let err = parse("events: Ir\nfn=f\n1 lots\n").unwrap_err();
        assert!(format!("{err:#}").contains("line 3"), "{err:#}");
    }

    #[test]
    fn a_file_without_events_is_not_a_profile() {
        assert!(parse("hello\n").is_err());
    }

    fn origin(runner: &str) -> Origin {
        Origin {
            runner: runner.into(),
            commit: Some("0123456789abcdef".into()),
            bench: "startup".into(),
            subject: "self".into(),
        }
    }

    /// No notes to check against.
    fn unrecorded(_: &Origin) -> Option<u64> {
        None
    }

    #[test]
    fn the_origin_is_read_back_from_the_description() {
        let dir = tempfile::tempdir().unwrap();
        let dest = path_for(dir.path(), "startup", "self").unwrap();
        write(&dest, BASE.as_bytes(), &origin("gha-linux-x64")).unwrap();
        let p = Profile::load(&dest).unwrap();
        assert_eq!(p.origin(), Some(origin("gha-linux-x64")));
        assert_eq!(p.runner().as_deref(), Some("gha-linux-x64"));
        // The rest is untouched.
        assert_eq!(p.total, parse(BASE).unwrap().total);
        // And a profile tak did not write has none.
        assert_eq!(parse(BASE).unwrap().origin(), None);
    }

    /// A value cannot add a `desc:` line of its own, or a cost line.
    #[test]
    fn an_origin_stays_on_its_own_lines() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("p");
        let o = Origin {
            runner: "r\nfl=x\nfn=y\n1 999".into(),
            ..origin("r")
        };
        write(&dest, BASE.as_bytes(), &o).unwrap();
        let p = Profile::load(&dest).unwrap();
        assert_eq!(p.total, 1_500);
        assert!(!p.functions.contains_key("y"));
    }

    #[test]
    fn a_name_that_is_not_a_plain_file_name_is_refused() {
        let dir = Path::new("/p");
        assert_eq!(
            path_for(dir, "startup", "self").unwrap(),
            Path::new("/p/startup/self.cachegrind.out")
        );
        for (bench, subject) in [
            ("..", "x"),
            ("a/b", "x"),
            ("a", ""),
            ("a", "."),
            ("a", "b\\c"),
            ("a\nb", "x"),
            ("a", "x\r"),
            ("a\0", "x"),
        ] {
            assert!(
                path_for(dir, bench, subject).is_err(),
                "{bench:?} {subject:?}"
            );
        }
    }

    #[test]
    fn a_self_subject_is_labelled_by_its_benchmark() {
        assert_eq!(label("startup/self.cachegrind.out"), "startup");
        assert_eq!(
            label("startup/hyperfine.cachegrind.out"),
            "startup (hyperfine)"
        );
        assert_eq!(label("mine.cachegrind.out"), "mine");
    }

    /// A name from a pull request's `tak.toml` stays a label: it cannot open
    /// a heading, a link or an HTML tag in the step summary.
    #[test]
    fn a_label_cannot_add_markdown() {
        assert_eq!(
            label("x\n## All clear/[ok](http:\\/e)<b>.cachegrind.out"),
            "x \\#\\# All clear (\\[ok\\](http:\\\\/e)\\<b\\>)"
        );
        assert_eq!(cell("a\nb`c"), "`a b'c`");
    }

    #[test]
    fn directories_pair_on_benchmark_and_subject() {
        let (b, h) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for (dir, text, subjects) in [
            (&b, BASE, &["self", "only-base"][..]),
            (&h, HEAD, &["self", "only-head"][..]),
        ] {
            for s in subjects {
                write(
                    &path_for(dir.path(), "startup", s).unwrap(),
                    text.as_bytes(),
                    &origin("r"),
                )
                .unwrap();
            }
        }
        let (pairs, unpaired) = load(b.path(), h.path()).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].label, "startup");
        assert_eq!(pairs[0].head.total, 2_110);
        assert_eq!(unpaired.base, ["startup/only-base.cachegrind.out"]);
        assert_eq!(unpaired.head, ["startup/only-head.cachegrind.out"]);
        assert!(load(b.path(), &b.path().join("startup/self.cachegrind.out")).is_err());
    }

    #[test]
    fn the_report_accounts_for_the_whole_change() {
        let pair = Pair {
            label: "startup".into(),
            base: parse(BASE).unwrap(),
            head: parse(HEAD).unwrap(),
        };
        let md = markdown(&[pair], &Unpaired::default(), 2, &unrecorded);
        assert!(
            md.contains("1,500 → 2,110 instructions (+610, **+40.67%**)"),
            "{md}"
        );
        assert!(
            md.contains("| `mycli::render` | **+900** | 100 | 1,000 | `src/render.rs` |"),
            "{md}"
        );
        assert!(md.contains("| `mycli::parse_args` | **-300** |"), "{md}");
        // 50 - 40: the two not shown.
        assert!(md.contains("| 2 more | +10 |"), "{md}");
        assert!(!md.contains("WARNING"), "{md}");
    }

    #[test]
    fn different_runners_are_called_out() {
        let mut base = parse(BASE).unwrap();
        let mut head = base.clone();
        base.desc.push("tak runner: a".into());
        head.desc.push("tak runner: b".into());
        let pair = Pair {
            label: "x".into(),
            base,
            head,
        };
        let md = markdown(&[pair], &Unpaired::default(), 10, &unrecorded);
        assert!(
            md.contains("different runner classes (`a` and `b`)"),
            "{md}"
        );
        assert!(
            md.contains("No function's instruction count changed."),
            "{md}"
        );
    }

    /// A profile replaced by a later run on the same commit no longer
    /// describes the count the gate used, and the report says so. One that
    /// matches, or has nothing recorded, says nothing.
    #[test]
    fn a_profile_that_is_not_the_recorded_count_is_called_out() {
        let with_origin = |text: &str| {
            let mut p = parse(text).unwrap();
            p.desc = vec![
                "tak runner: r".into(),
                "tak commit: 0123456789abcdef".into(),
                "tak bench: startup".into(),
                "tak subject: self".into(),
            ];
            p
        };
        let pair = Pair {
            label: "startup".into(),
            base: with_origin(BASE),
            head: with_origin(HEAD),
        };
        let asked = std::cell::RefCell::new(Vec::new());
        // The base matches its note; the head's note is lower.
        let recorded = |o: &Origin| {
            asked.borrow_mut().push(o.clone());
            Some(if asked.borrow().len() == 1 {
                1_500
            } else {
                2_000
            })
        };
        let md = markdown(&[pair], &Unpaired::default(), 10, &recorded);
        assert!(
            md.contains(
                "The head profile totals 2,110 instructions, but the lowest count recorded \
                 for `0123456789ab` in git notes is 2,000."
            ),
            "{md}"
        );
        assert!(!md.contains("The base profile"), "{md}");
        assert_eq!(asked.borrow()[0].bench, "startup");
        assert_eq!(asked.borrow()[0].subject, "self");
    }

    /// cachegrind writes no indentation, but an indented cost line must not
    /// be skipped as unrecognised while the total stays as it was.
    #[test]
    fn an_indented_line_is_read_like_any_other() {
        let p = parse("events: Ir\n  fl=a.c\n\tfn=f\n   1 5\n  2 7  \n summary: 12\n").unwrap();
        assert_eq!(p.functions["f"].ir, 12);
        assert_eq!(p.functions["f"].files["a.c"], 12);
    }

    #[test]
    fn a_pipe_in_a_name_does_not_break_the_table() {
        assert_eq!(cell("f::{{closure}}|x"), "`f::{{closure}}\\|x`");
    }
}
