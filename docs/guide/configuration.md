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
