# Explain an instruction-count change

::: warning Pre-v1 software
`--profile-dir`, `tak explain`, the profile layout and the report format are new and may change
incompatibly between releases.
:::

`tak compare` says that a benchmark's instruction count rose by some percentage. It does not say
where. cachegrind, which produces the count, also records how many instructions each function
retired, and tak can keep that profile and compare two of them.

Timing profilers can't give this for a short command: a sampled profile of a 10ms run is mostly
noise. These per-function counts come from the same deterministic measurement as the total, so a
function whose count moved did more or less work.

## Keep the profiles

Pass `--profile-dir` to `tak run`:

```sh
tak run --profile-dir profiles
```

Each subject whose instructions were counted gets one file:

```
profiles/<bench>/<subject>.cachegrind.out
```

A single-command benchmark's subject is the name it is recorded under: `self`, or `TAK_TOOL`
when that is set. Benchmark and subject names must be usable as file names: a name containing
`/`, `\` or a control character, or one that is `.` or `..`, stops the run before anything is
measured.

The file is cachegrind's own output format with `desc: tak runner: …`, `desc: tak commit: …`,
`desc: tak bench: …` and `desc: tak subject: …` lines added at the top, so `cg_annotate` and
similar tools can read it too. tak runs cachegrind three times per subject and reports the
minimum; the profile kept is from that run, so its total is the recorded `instructions` value.
If the profile cannot be kept, tak prints a warning and keeps the instruction count.

tak overwrites the files it writes and leaves everything else in the directory alone. Use a
fresh directory for each run, or a benchmark that has since been removed will still be found
there. Measuring the same commit again also replaces its profile, while `tak compare` uses the
lowest count recorded for the commit. `tak explain` looks up the lowest count the local git
notes hold for the profile's commit, benchmark, subject and runner class, without fetching.
When that differs from the profile's own total, it prints a warning that the profile is not
from the run that count came from.

Profiles are not stored in git notes, and `tak artifact export` does not include them. A profile
is hundreds of kilobytes, while notes hold one-line records.

## Compare two sets

```sh
tak explain base-profiles head-profiles
```

`tak explain` matches profiles by `<bench>/<subject>` and prints markdown for each pair: the
total change, then the functions whose counts changed, largest change first in either
direction. The last row adds up the functions not shown, so the table accounts for the whole
change. `--top N` sets how many functions are listed (default 10). Two single profile files
work as well as two directories.

`tak explain` only reports. It exits zero whatever the numbers are; the gate is `tak compare`.

For example, a small C program whose `render` function was changed to loop five times as often,
built with `-g` and measured before and after the change:

```sh
tak run --bench startup --profile-dir base -- ./mycli hello
# rebuild mycli with the change
tak run --bench startup --profile-dir head -- ./mycli hello
tak explain base head
```

```md
## Where the instructions went

### startup

124,476 → 144,457 instructions (+19,981, **+16.05%**).

3 function(s) changed:

| function | Δ | base | head | file |
|---|---:|---:|---:|---|
| `render` | **+20,000** | 5,007 | 25,007 | `/home/t/demo/demo.c` |
| `_itoa_word` | **-14** | 154 | 140 | `./stdio-common/./stdio-common/_itoa.c` |
| `_IO_default_xsputn` | **-5** | 99 | 94 | `./libio/./libio/genops.c` |
```

The two small changes in glibc are `printf` formatting a different number.

## What the numbers mean

- **Self cost.** Each function's count is the instructions retired in its own code, not
  in the functions it calls. Code inlined into a function counts as that function's own, so a
  change in what the compiler inlines moves instructions between functions without changing
  the total.
- **Matched by name.** Functions are matched by name alone, not by source file, because
  cachegrind charges a line to the file it came from and the same build in a different
  checkout would otherwise match nothing. A renamed function shows as one removed and one
  added. Two functions with the same name, such as C `static` functions in different files,
  are added together.
- **Symbols.** Code without symbols shows as `???`, all of it together. A stripped binary,
  such as a Rust release build with `strip = true`, produces little else. Profile a build that
  keeps its symbols; debug line tables are what fill in the file column.
- **One process.** cachegrind profiles the process tak starts and nothing it starts in turn,
  which is also true of the instruction count. A subject that is a shell script is profiled as
  the shell.
- **Runner class.** Library code differs between machines: a different glibc, or a different
  CPU, selects a different `memcpy`. When the two profiles were taken on different runner
  classes, the report says so above that benchmark's table.

## In CI

The base profile has to come from measuring the base. The measurements in git notes carry
only totals, so nothing recorded on main can be explained later unless its profiles were kept
somewhere else. There are two ways to get a base profile, and both need work outside tak:

**Measure both revisions in the pull request job.** Build and measure the base, then the head,
on the same runner. This doubles the measuring time.

```sh
git checkout "$BASE_SHA"
# build
tak run --profile-dir "$RUNNER_TEMP/base"
git checkout "$HEAD_SHA"
# build
tak run --profile-dir "$RUNNER_TEMP/head" --record
tak explain "$RUNNER_TEMP/base" "$RUNNER_TEMP/head" >> "$GITHUB_STEP_SUMMARY"
```

**Keep main's profiles as a CI artifact.** Have the job that records main pass
`--profile-dir` and upload the directory, named after the commit. A pull request job then
downloads the artifact for its base commit. That needs the base commit to have been measured
on main, on the same runner class, within the artifact retention period. tak does not look
for or download the artifact.
