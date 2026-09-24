# Benchmark configuration

tak searches upward from the working directory for `tak.toml`. Commands run relative to the
directory containing that file, so CI and local runs use the same paths.

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

- Subjects inherit the benchmark's `runs`, `warmup`, `prepare`, `dir` and `env`. A subject's
  own `prepare` replaces the benchmark's, and its `env` entries override matching keys.
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
budget = "30s"   # wall time to spend per subject, prepare included (default 30s)
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
in hyperfine's `--export-json` shape, with `bench` and `subject` fields added:

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
`prepare`, `dir`, `env`, `vars`). It is a separate table because `[env]` already holds
`env.deny` and `env.allow`.

## Templates

<!-- tera syntax looks like Vue interpolation; v-pre stops VitePress evaluating it. -->
::: v-pre
Values in `cmd`, `prepare`, `dir`, `env` and `vars` are [tera](https://keats.github.io/tera/)
templates, the same syntax mise uses. tak renders them itself before anything runs, so a
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
