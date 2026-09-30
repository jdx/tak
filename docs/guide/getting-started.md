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

## Measure a local change

`tak compare` compares commits whose measurements were recorded in git notes. While you are
editing, there is no commit yet. Save a named local baseline instead, make the change, and
measure against it:

```sh
tak run --save-baseline before
# edit and rebuild
tak run --baseline before
```

The second run prints the same table `tak compare` prints, with the saved measurement as the
base and this run as the head. For a benchmark named `loop` whose change removed a quarter of
its work:

```text
  compared against baseline `before` (/tmp/demo/.git/tak/baselines/before.jsonl)

| benchmark | instructions | Δ | wall (min) | Δ |
|---|---:|---:|---:|---:|
| loop | 23,142,252 → 17,378,752 | **-24.90%** | 1.53 → 1.20ms | -21.59% |

No instruction-count regression above 1%.
```

Without Valgrind the instruction columns show `—` and only wall clock is compared. Wall clock
is never gated.

- Baselines are stored under the repository's git directory, in `tak/baselines/NAME.jsonl`.
  They are never committed or pushed, `git clean` does not remove them, and every worktree of
  a clone sees the same ones. tak does not create baselines outside a git repository.
- `--baseline` only reads. It never writes to `refs/notes/tak`. Add `--record` to record the
  run as well, or `--save-baseline NAME` to replace a baseline after comparing against it.
  With `--gate`, a baseline is not replaced by a run that failed the gate against it. For
  example, `tak run --baseline good --save-baseline good --gate` keeps `good` when the gate
  fails, so a retry doesn't compare the regression with itself. The error says so. Saving
  under a different name happens either way.
- `--save-baseline` replaces what the baseline held for the benchmarks this run measured and
  keeps the rest, so `tak run --bench startup --save-baseline before` updates one benchmark.
  Like `--record`, it saves nothing when a subject fails or a check fails. With both flags,
  the baseline is saved first; if recording then fails, the error says so, and re-running the
  same command finishes the job without duplicating anything in the baseline.
- The report covers only the benchmarks this run measured, on this run's runner class. One
  baseline can hold several runner classes (for example, saved once with `--runner laptop` and
  once in a container). Each run is compared only with its own class's measurements, and the
  other classes are ignored. When a benchmark was saved only under other runner classes, a
  warning names them and nothing is compared for it.
- `--baseline` reports and exits 0 whatever the numbers say. If a check failed, the report
  says so under the table, because a subject that skips its work usually looks faster.
- Add `--gate` to fail when an instruction count rose beyond its gate. Each benchmark is held
  to the same gate `tak compare` would use, from `[gate]` and the benchmark's own `gate`
  table (see [regression gate](/guide/configuration#regression-gate)). That includes the
  `min_delta` floor. A report-only benchmark (`gate = { enabled = false }`) is flagged but
  never fails `--gate`. When every benchmark measured or compared is report-only, `--gate`
  passes and prints a note that nothing was gated.
- An ad-hoc run (`tak run --baseline NAME -- CMD`) doesn't depend on the benchmarks
  `tak.toml` declares, so an invalid benchmark in the file can't block the report. With
  `--gate` it reads the benchmark's own gate, so `--gate` needs a `tak.toml` that loads. If it
  doesn't, the run fails before measuring and shows the configuration error.
- `--gate` also fails for a gated benchmark it could not check:
  - nothing ran;
  - no instruction count appears on both sides;
  - a benchmark measured in this run cannot be compared on this runner class: its count
    failed or counters were off, it is new since the baseline was saved, or the baseline
    holds it only for other runner classes. A count that was requested and failed while
    Valgrind was present always counts here, even for a benchmark new since the baseline;
  - a check failed.

  A gate that passed in those cases would pass changes it never measured.
- Saves to one baseline from several worktrees at once are serialised with a lock file in the
  same directory, so each run's benchmarks are kept.
- An older tak refuses, before measuring, to save over a baseline that holds records written
  by a newer tak. It can't tell which benchmark those records belong to, and writing beside
  them could leave a stale value that the newer tak would later compare against. Save under
  another name or upgrade.
- A mistyped `--baseline` name fails before anything is measured, and the error lists the saved
  baselines. To delete a baseline, remove its file.

Continue with [adopting tak in a project](/guide/adopting),
[benchmark configuration](/guide/configuration), or
[recording results in git notes](/guide/ci).
