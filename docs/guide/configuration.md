# Benchmark configuration

tak searches upward from the working directory for `tak.toml`. Commands run relative to the
directory containing that file, so CI and local runs use the same paths. `tak run --config PATH`
reads a named file instead, including the settings it holds, and commands then run relative to
that file. That's useful for a second set of benchmarks kept apart from the main ones.

Editors that read [JSON Schema](https://json-schema.org), such as Even Better TOML or taplo, can
complete and check `tak.toml` against `https://tak.jdx.dev/schema/tak.json`. Add this as the
file's first line:

```toml
#:schema https://tak.jdx.dev/schema/tak.json
```

To see exactly what a run would do, with defaults and shared subjects applied, templates
rendered and command-line overrides taken into account, run it with `--dry-run`. Nothing is
measured:

```sh
tak run --bench install --dry-run
```

## Benchmarks

Each `[bench.NAME]` table needs a command:

```toml
[bench.startup]
cmd = ["./target/release/mycli", "--version"]
runs = 10
warmup = 2
```

`cmd` may be an argument list or a whitespace-split string. tak deliberately never starts a
shell because shell startup would add work and variance to the subject.

Command-line values override the file:

```sh
tak run --bench startup --runs 20 --warmup 3
```

## Resetting state before each sample

`prepare` runs before every sample, warmups included, and is not timed. Use it when each sample
has to start from the same state, such as an install benchmark that needs an empty
`node_modules`:

```toml
[bench.install]
cmd = ["./target/release/mycli", "install"]
prepare = ["sh", "-c", "rm -rf node_modules"]
dir = "fixtures/app"
env = { MYCLI_OFFLINE = "1" }
```

`prepare` uses the same syntax as `cmd`, and there is still no implicit shell: write
`["sh", "-c", "…"]` when you need one. A shell costs nothing here because `prepare` is outside
the measurement. `dir` is relative to `tak.toml` and only sets the working directory. A program path containing a `/`, such as `./target/release/mycli`, is still found relative to `tak.toml`; a bare name like `mycli` is looked up on `PATH`. Variables in `env` are set after tak removes
the ones in `env.deny`, so a variable written here reaches the command even when it is denied
by default.

## Setting up once before measuring

`setup` runs once for each subject of a benchmark, before that benchmark takes its first
sample, and is not timed. Use it for work every sample needs but that only has to happen once,
such as cloning a fixture and committing each tool's configuration to it, or priming a cache:

```toml
[defaults]
dir = ".work/{{ subject }}"
setup = ["./bench/setup.sh", "{{ subject }}", ".work/{{ subject }}"]
prepare = ["sh", "-c", "git reset -q --hard && git clean -qfd"]

[subject.hk]
cmd = ["hk", "check", "--all"]

[subject.lefthook]
cmd = ["lefthook", "run", "check", "--all-files"]

[bench.check-all]
subjects = ["hk", "lefthook"]
```

Here `setup.sh` clones the fixture into `.work/hk` or `.work/lefthook`, commits that tool's
configuration and runs the tool once to fill its cache. `prepare` then only has to reset the
checkout before each sample.

- **It runs in the directory holding `tak.toml`, not in `dir`.** Setup usually creates or
  recreates `dir`, and it can't start inside a directory that doesn't exist yet or that it's
  about to delete. `dir` is written relative to `tak.toml` too, so the same path works as an
  argument to `setup`, as above. To run something inside `dir`, `cd` into it:
  `["sh", "-c", "cd .work/hk && hk check --all"]`. Otherwise `setup` is like `prepare`: the same
  syntax, templates, the subject's `env`, and no implicit shell.
- **It runs before the benchmark's sampling starts.** Every subject's setup runs, in name
  order, before the benchmark's first warmup, so no sample shares the machine with a setup. It doesn't count
  toward `budget` or `runs = "auto"`'s sizing or the progress estimate. Progress shows which
  subject is being set up.
- **It runs once per benchmark.** A shared subject listed by two benchmarks is set up again
  for the second one, because the first benchmark's samples may have changed its state. Make
  an expensive setup reuse what it built before when that is safe.
- **It only runs for subjects that will be measured.** A subject left out with `--subject` or
  switched off by `when` isn't set up, and `--dry-run` prints `setup` without running it.
- **A failing setup drops the subject**, as a failing `prepare` does: tak measures the rest,
  exits non-zero, and `--record` writes nothing.
- **Its output is hidden.** When it fails, tak reports the last line of its stderr. Run the
  command by hand to see the rest.

Settings stack the same way as `prepare`: a subject's own `setup` replaces the benchmark's,
which replaces the one in `[defaults]`. There is no matching `teardown`. The next run's setup
can clean up whatever the last one left, and keeping it around lets you inspect a subject's
directory after a run.

## Checking every sample

A fast time is only worth reporting if the command did the right work. `check` runs after every
timed sample, untimed, in the same directory and environment as the command. Exit status 0 means
the sample passed; anything else means it failed. Use it when a command's output can be wrong
some of the time, such as formatters that race when run concurrently on the same files:

```toml
[bench.fix]
runs = 20
warmup = 1
dir = "fixture"   # a git repository with tags `dirty` (unformatted) and `clean` (the expected result)
prepare = ["git", "reset", "--hard", "--quiet", "dirty"]
check = ["git", "diff", "--quiet", "clean"]

# Two fixers one after the other.
[bench.fix.subject.serial]
cmd = ["sh", "-c", "sed -i 's/foo/bar/' a.txt; sed -i 's/ *$//' a.txt"]

# The same two fixers at once, on the same file.
[bench.fix.subject.parallel]
cmd = ["sh", "-c", "sed -i 's/foo/bar/' a.txt & sed -i 's/ *$//' a.txt & wait"]
```

`git diff --quiet clean` exits 1 when the working tree differs from the `clean` commit. In this
run the concurrent fixers lost an edit every time, and were faster for it:

```text
  fix: 2 subjects, interleaved (--seed 1)
  warning: fix (parallel): check failed after 20 of 20 samples (sample 1, 2, 3, 4, 5, 6, 7, 8, …); first: check `git` exited with exit status: 1
    parallel  min      1.96  p50      2.13  mean      2.15 ± 0.15     max      2.47 ms  n=20  checks 0/20
    serial    min      2.49  p50      2.72  mean      2.78 ± 0.21     max      3.24 ms  n=20  checks 20/20
```

- A failing check doesn't drop the subject, and without `--record` it doesn't fail the run.
  Every sample is kept, and the pass count is the result: a race that loses one sample in
  twenty shows up as `checks 19/20` on the subject's summary line. tak warns on stderr, listing
  the failed sample numbers and the last line the first failing check wrote to stderr. The
  summary doesn't say which time came from which sample, so it can't tell you whether the
  minimum is from a failed one. The export can.
- A check that can't be started at all, such as a mistyped program, is a mistake in `tak.toml`
  rather than a result. That drops the subject like a failing `cmd`.
- Warmups aren't checked, because they aren't kept. The check isn't counted toward a
  `runs = "auto"` budget, which is sized from prepare and the command alone.
- The instruction-count runs of a subject with `counters = true` are separate from the timed
  samples, and the check doesn't run after them.
- `check` uses the same syntax as `cmd` and `prepare`: no implicit shell, and a program path
  containing a `/` is found relative to `tak.toml`. It stacks like `prepare`, so a subject's
  own `check` replaces the benchmark's.
- `--export-json` adds `checks` to each result that has one:
  `{"passed": 18, "total": 20, "samples": [true, …]}`, with `samples` in the same order as
  `times`, so each verdict can be matched to its time. The hyperfine fields are unchanged.
- **`--record` writes nothing if any check failed.** Git notes keep timings but not
  verdicts, so the timings of a run with a failed check would be stored as if it had
  passed. `tak compare` keeps each metric's minimum and treats lower as better, and a pass
  rate fits neither rule, so it isn't stored alongside them. Instead, tak names each subject
  whose check failed and how many of its samples failed, writes no notes, and exits
  non-zero, as it does when a subject is dropped. `--export-json` is still written, with the
  verdicts.

## Comparing several programs

To compare programs against each other, declare them as subjects of one benchmark instead of
giving the benchmark a `cmd`:

```toml
[bench.install]
runs = 10
warmup = 1
prepare = ["sh", "-c", "rm -rf node_modules"]

[bench.install.subject.mycli]
cmd = ["./target/release/mycli", "install"]
dir = "fixtures/mycli"

[bench.install.subject.othertool]
cmd = ["othertool", "install"]
dir = "fixtures/othertool"
env = { HOME = "/tmp/othertool-home" }
runs = 5
```

tak interleaves the samples: every round takes one sample of each subject in a freshly shuffled
order, rather than every sample of one subject and then the next. See
[methodology](/guide/methodology#comparing-programs) for why.

- Subjects inherit the benchmark's `runs`, `warmup`, `setup`, `prepare`, `check`, `dir` and
  `env`. A subject's own `setup`, `prepare` or `check` replaces the benchmark's, and its `env`
  entries override matching keys.
- A subject with fewer `runs` than the others is spread evenly across the run.
- Each subject is recorded as its own series, with the subject name as the tool. Instruction
  counts are off for subjects unless they set `counters = true`, so another program's upgrade
  cannot trip the gate.
- If a subject fails, tak drops it and keeps measuring the others. The run then exits non-zero
  and `--record` writes nothing, because a partial set of measurements would look complete.

### Choosing the number of runs

Programs in one comparison can differ in speed by a factor of 50: a
300 ms install next to a 20 s one. A fixed `runs` either spends minutes on the slow program or
leaves the fast one under-sampled. `runs = "auto"` lets tak decide per subject:

```toml
[bench.install]
runs = "auto"
budget = "30s"   # wall time to spend per subject, prepare included, setup not (default 30s)
min_runs = 5     # never fewer (default 5)
max_runs = 50    # never more (default 50)
```

After the warmups, tak times each subject and gives it as many runs as fit in `budget`, within
`min_runs` and `max_runs`. With the defaults, a 1.7 s install gets 17 runs and a 21 s one gets
the minimum of 5. A subject with `warmup = 0` is sized from its first real sample, which still
counts toward its runs. Each sample is measured from the start of its `prepare` step, because
that's how long it actually takes.

`budget`, `min_runs` and `max_runs` can be set on the benchmark or per subject, and a subject
can still fix its own `runs`. `tak run --runs auto` switches a benchmark to auto for one run.
Because the counts depend on measured timings, `--seed` only repeats the same order when the
counts come out the same.

### Output

While a benchmark runs, tak shows progress on stderr: a bar in a terminal, or a plain line
every tenth of the way (or every 30 seconds) anywhere else, such as CI logs. The time
remaining is estimated separately for each subject from its own samples so far, so a slow
subject's remaining samples are counted at its own speed. Pass `--no-progress` to turn it off.

tak also warns on stderr when a subject's samples look suspect:
- **Slow outliers** (a modified z-score above 3.5; when over half the samples are identical,
  anything more than 25% away from them): something else ran, or the command's work varies.
  These don't change the minimum.
- **Fast outliers:** these can *be* the minimum, so check the command did the same work every
  time before trusting the headline number.
- **A slow first sample:** the first timed sample took over twice the median of the rest, so
  the warmup didn't fill some cache.

Nothing is dropped or adjusted. A warning means the comparison may be worth running again.
A multi-subject benchmark prints one summary line per subject. This is the output of a real run
comparing `sleep 0.1` (`fast`) with `sleep 0.8` (`slow`), using `runs = "auto"`, `budget = "3s"`
and `min_runs = 3`:

```text
  install: 2 subjects, interleaved (--seed 3)
    fast  min    102.62  p50    106.99  mean    106.49 ± 1.50     max    108.41 ms  n=28
    slow  min    803.47  p50    807.21  mean    806.24 ± 2.44     max    808.05 ms  n=3
```

Every multi-subject run prints its seed. Pass it back with `--seed` to repeat an order.
`--subject NAME` limits a run to the named subjects, and `--export-json PATH` writes every sample
in hyperfine's `--export-json` shape, with `bench` and `subject` fields added to each result
(and `checks`, for a subject with a [`check`](#checking-every-sample)).
The file also records how the run was made: `tak_version`, `seed`, `runner` and `time`.

```sh
tak run --bench install --seed 1234 --export-json results.json
```

## Sharing settings between benchmarks

A comparison usually runs the same programs in several scenarios, such as a warm install
and a cold one. Declare each program once as a top-level subject and list it from each
benchmark:

```toml
[defaults]
runs = "auto"
min_runs = 5

[subject.mycli]
cmd = ["./target/release/mycli", "install"]

[subject.othertool]
cmd = ["othertool", "install"]
env = { OTHERTOOL_CACHE = "/tmp/othertool" }

[bench.warm]
subjects = ["mycli", "othertool"]
prepare = ["sh", "-c", "rm -rf node_modules"]

[bench.cold]
subjects = ["mycli", "othertool"]
prepare = ["sh", "-c", "rm -rf node_modules ~/.cache/mycli /tmp/othertool"]

# A benchmark can override a shared subject, or add one of its own.
[bench.cold.subject.mycli]
cmd = ["./target/release/mycli", "install", "--no-cache"]
```

Settings stack from least to most specific: `[defaults]`, then the benchmark, then the
shared `[subject.NAME]`, then the benchmark's own `[bench.B.subject.NAME]`. Each layer's
setting replaces the one before, except `env` and `vars`, which merge key by key. `[defaults]`
takes every benchmark setting (`runs`, `warmup`, `budget`, `min_runs`, `max_runs`,
`setup`, `prepare`, `check`, `dir`, `env`, `vars`). It is a separate table because `[env]`
already holds `env.deny` and `env.allow`.

## Templates

<!-- tera syntax looks like Vue interpolation; v-pre stops VitePress evaluating it. -->
::: v-pre
Values in `cmd`, `setup`, `prepare`, `check`, `dir`, `env` and `vars` are
[tera](https://keats.github.io/tera/) templates, the same syntax mise uses. tak renders them itself before anything runs, so a
command can use a path that only exists at run time and still be a plain argument list,
without a shell:

```toml
[defaults]
dir = "{{ env.BENCH_DIR }}/project-{{ subject }}"
env = { HOME = "{{ env.BENCH_DIR }}/home-{{ subject }}" }

[subject.mycli]
cmd = ["{{ env.MYCLI_BIN }}", "install", "--lockfile", "{{ vars.lockfile }}"]
vars = { lockfile = "mycli.lock" }
```

A template can use:

| name | value |
|---|---|
| `env` | tak's own environment, such as `{{ env.HOME }}` |
| `bench` | the benchmark's name |
| `subject` | the subject's name (`self` for a single-command benchmark) |
| `vars` | the subject's `vars` tables, merged like `env`; not passed to the command |

`vars` values are rendered first, so they can build on `env`. Filters work as in mise, for
example `{{ env.MYCLI_BIN | default(value="mycli") }}`. Using a variable that isn't set is
an error when the benchmark is loaded, before any sample runs, rather than an empty string
that sends a command to the wrong path. Template syntax is checked for the whole file up
front; values are rendered only for the benchmarks being run, so a variable needed by one
benchmark doesn't have to be set to run another.
:::

## Conditions

`when` limits a benchmark or subject to the times an [expr](https://expr-lang.org) condition
holds, the expression language mise uses for its own conditions. A comparison can list every
tool and leave out whichever isn't installed:

```toml
[subject.vlt]
when = '(env.VLT_BIN ?? "") != ""'
cmd = ["{{ env.VLT_BIN }}", "install"]

[bench.linux-only]
when = 'os == "linux"'
cmd = ["./target/release/mycli", "--version"]
```

A condition can use `env` (tak's environment), `os` and `arch` (as Rust names them: `linux`,
`macos`, `x86_64`, `aarch64`), `ci` (whether `CI` is set to anything but empty or `false`),
`bench`, and `subject`. It must evaluate to `true` or `false`. Conditions are decided before
templates are rendered, so a skipped subject's variables don't need to be set. tak prints each
skipped benchmark or subject on stderr. A subject asked for with `--subject` that no selected
benchmark will run is an error. If `when` switches off everything selected, `--record` and
`--export-json` fail rather than succeed with nothing written. A `when` in a benchmark's own subject table replaces the shared subject's; a table without one keeps the shared condition, like every other setting. Conditions are only evaluated for the benchmarks and subjects being run, so an unrelated subject's condition can't stop a `--subject` run.
Conditions are syntax-checked when `tak.toml` is loaded.

## Environment and runner settings

tak removes known sources of non-determinism from measured commands. Inspect every resolved
setting and its source with:

```sh
tak settings --docs
```

The most important setting is the runner class. Measurements from different runner classes
must never share a series:

```toml
[runner]
class = "gha-linux-x64-rust-1.90"
```

Use an explicit class when a compiler, base image, or other invisible input changes the
measurement without changing the machine name.

## Regression gate

`tak compare` fails when an instruction count rises by more than the gate. The default is 1%,
which leaves room above the observed instruction-count noise without making timing part of the
decision:

```toml
[gate]
pct = 0.5
```

The command line and environment can override the project value:

```sh
tak compare origin/main --gate-pct 2
TAK_GATE_PCT=2 tak compare origin/main
```

Only instruction counts are gated. Wall-clock changes are displayed but never fail the
comparison. Use `tak compare --no-gate` when a report must always exit successfully.

## Environment filtering

tak removes known sources of non-determinism from the measured command's environment. The
project setting replaces the default deny list, so repeat the defaults when adding
project-specific credentials or configuration:

```toml
[env]
deny = ["GITHUB_TOKEN", "GH_TOKEN", "MYCLI_UPDATE_CHECK"]
```

`allow` can opt a listed name back in without restating the deny list. It subtracts names from
the deny list; it does not add variables to the environment. Passing credentials or network
configuration through to a subject makes its measurements depend on state outside the
repository.

## Report credit

Generated comparison reports name tak in their footer by default. Disable that line when the
surrounding report already provides the context:

```toml
[report]
credit = false
```

Every setting follows the same precedence: command-line flag, environment variable,
`tak.toml`, then the built-in default. `tak settings --docs` prints the resolved value, its
source, every supported source, and the full setting documentation.
