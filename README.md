<p align="center">
  <img src="assets/tak-icon-tile.svg" alt="tak logo" width="140" height="140">
</p>

<h1 align="center">tak</h1>

<p align="center">
  <em>A tachometer for code.</em>
</p>

> [!CAUTION]
> ## Pre-v1 software
>
> **tak is pre-v1. Its CLI, configuration, storage format, and behavior are not finalized.**
>
> Breaking changes may land between releases, including changes that require existing
> configuration or recorded data to be updated. If you need a stable benchmark tool, use
> [hyperfine](https://github.com/sharkdp/hyperfine). If you need CI benchmark tracking, use
> [Bencher](https://bencher.dev) or [CodSpeed](https://codspeed.io).

## Why instruction counts

Wall-clock time on a shared CI runner has roughly the same noise floor as the regressions
people want to catch. tak asks whether retired instruction counts can provide a deterministic
signal instead.

Measured on a 32-core Linux host:

| metric | quiet host | under 32-way CPU contention | median drift |
|---|---|---|---|
| **instructions** (cachegrind) | **0.008–0.027% CV** | **0.011–0.021% CV** | **≤0.035%** |
| wall clock | 3.9–20.6% CV | 14.2–19.2% CV | **+147% to +164%** |

That produces one narrow rule: **gate on instruction counts; report wall time without gating
on it.** Syscall counts and peak RSS move with thread scheduling and are not deterministic
enough for a tight threshold.

Measurements stay in the repository as JSON lines under `refs/notes/tak`, merged with git's
`cat_sort_uniq` strategy. There is no database, account, or hosted service.

Read [the methodology](https://tak.jdx.dev/guide/methodology) for the measurements, limitations,
and reasoning.

## What exists

- `tak run` measures wall time and, where Valgrind is available, instruction counts
- `tak.toml` declares repeatable benchmarks for local and CI runs
- a benchmark can also record custom metrics, such as a file's size in bytes or a number a
  command prints. They are stored and reported beside the timings, and never gated
- `tak run --record`, `tak push`, and `tak history` store results in git notes
- `tak artifact export` and `tak artifact publish` hand measurements from a
  read-only CI job to a separately trusted publisher
- `tak compare` reports changes and gates only on instruction counts. It fails when nothing
  was measured on both sides, unless `--allow-empty` or `--no-gate` is given. Under
  `--allow-empty` a regression still fails; `--no-gate` never fails on the comparison,
  though errors such as an invalid `tak.toml` still do
- `tak compare --accept BENCH` accepts an intentional regression in one named benchmark while
  every other benchmark still gates; `Tak-Accept:` commit trailers do the same once a project
  opts in
- `tak run --save-baseline NAME` and `tak run --baseline NAME` compare a local, uncommitted
  change against a saved measurement, without touching git notes
- `tak detect` reports instruction-count steps that already landed on a branch, and fails
  when the newest recorded commit introduced one or when nothing could be compared
- `tak log` shows each benchmark's measurements over first-parent history, as Markdown or as
  a self-contained HTML report
- `tak backfill` measures published release binaries, or builds and measures past commits
  with `--commits`, to bootstrap history

tak itself does not post to pull requests. [jdx/tak-action](https://github.com/jdx/tak-action),
which is also pre-v1, runs `tak compare` on a pull request and reports the result as a sticky
comment and a check run. See [Adopt tak in a project](https://tak.jdx.dev/guide/adopting#use-the-github-action).

Change-point detection does not exist. `tak detect` compares consecutive recorded points
against the gate; it is not statistical change-point detection.

## Documentation

- [Getting started](https://tak.jdx.dev/guide/getting-started)
- [Adopt tak in a project](https://tak.jdx.dev/guide/adopting)
- [Benchmark configuration](https://tak.jdx.dev/guide/configuration)
- [CI and git notes](https://tak.jdx.dev/guide/ci)
- [CLI reference](https://tak.jdx.dev/cli/)

The crate is `tak-cli`; the binary is `tak`. Releases are automated as described in
[RELEASING.md](RELEASING.md).

## Development

Use mise so local and CI commands stay aligned:

```sh
mise run build
mise run test
mise run lint
mise run ci
```

Instruction-count tests require Valgrind. Run `tak doctor` to see what is available on the
current host.

## License

MIT
