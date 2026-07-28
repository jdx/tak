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
