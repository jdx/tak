# Getting started

::: warning Pre-v1 software
tak's CLI, configuration, storage format, and behavior are not finalized. Expect breaking
changes between releases.

These docs are currently AI slop and have not been fully reviewed. They will be reviewed and
finished later.
:::

## Install

Install the published crate:

```sh
cargo install tak-cli
```

Prebuilt binaries may also be available on the
[GitHub releases page](https://github.com/jdx/tak/releases).

Instruction counting requires [Valgrind](https://valgrind.org) and is unavailable on Apple
Silicon and Windows. Without it, tak records timing only.

## Measure one command

Put the command after `--` so tak never interprets its flags:

```sh
tak run -- mycli --version
```

Use `--bench` to give an ad-hoc measurement a stable name:

```sh
tak run --bench startup -- mycli --version
```

## Declare repeatable benchmarks

Create `tak.toml` in the project root:

```toml
[bench.startup]
cmd = ["./target/release/mycli", "--version"]

[bench.help]
cmd = "mycli --help"
runs = 10
```

Then run every declaration or select one:

```sh
tak run
tak run --bench startup
```

String commands are split on whitespace. There is no shell, quoting, globbing, or pipeline
syntax. Use an argument list when boundaries matter.

Continue with [adopting tak in a project](/guide/adopting),
[benchmark configuration](/guide/configuration), or
[recording results in git notes](/guide/ci).
