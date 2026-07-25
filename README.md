<p align="center">
  <img src="assets/tak-icon-tile.svg" alt="tak logo" width="140" height="140">
</p>

<h1 align="center">tak</h1>

<p align="center">
  <em>A tachometer for code.</em>
</p>

> [!CAUTION]
> ## 🚧 GO AWAY 🚧
>
> **This is a half-baked experiment. Do not use it. Do not depend on it. Do not package it.**
>
> It is vibe-coded slop that exists so one person can find out whether an idea is any good.
> It is not a product, it has no support, no stability guarantees, and no roadmap you should
> trust. Interfaces will change without warning. It may be renamed, rewritten from scratch,
> or deleted outright. If it eats your afternoon, that is on you — you were warned in a very
> large box at the top of the README.
>
> **Please do not file issues or open pull requests.** They will most likely be closed unread.
> There is nothing here to contribute to yet.

## If you actually want to benchmark something

Use one of these instead. They are real, finished, maintained software written by people who
care about your use case:

| you want | use |
|---|---|
| Benchmark a command on the CLI | **[hyperfine](https://github.com/sharkdp/hyperfine)** |
| ...with hardware counters, on Linux | **[poop](https://github.com/andrewrk/poop)** |
| Track benchmarks in CI over time | **[Bencher](https://bencher.dev)** |
| Zero-noise CI benchmarking, hosted | **[CodSpeed](https://codspeed.io)** |
| Chart benchmarks from a GitHub Action | **[github-action-benchmark](https://github.com/benchmark-action/github-action-benchmark)** |
| Detect regressions in a noisy series | **[Nyrkiö](https://nyrkio.com)** / [Apache otava](https://github.com/apache/otava) |
| Store perf measurements in git notes | **[git-perf](https://github.com/kaihowl/git-perf)** |

Seriously. Go use hyperfine.

---

## Still reading?

Fine. Here is the idea, so you can decide it's uninteresting and leave.

**Premise:** wall-clock time on a shared CI runner has a noise floor of roughly 10–20%, which
is the same size as the regressions people want to catch. Every threshold-based CI benchmark
therefore either cries wolf or sees nothing, and eventually gets muted.

Measured on a 32-core Linux box, six `mise` startup commands, comparing coefficient of
variation across metrics:

| metric | quiet host | under 32-way CPU contention | median drift, quiet → loaded |
|---|---|---|---|
| **instructions** (cachegrind) | **0.008–0.027%** | **0.011–0.021%** | **≤0.035%** |
| syscall count | 0.59–1.09% | 1.06–1.70% | ~1% |
| max RSS | 0.75–1.09% | 0.51–1.48% | ~1% |
| wall clock (hyperfine) | 3.9–20.6% | 14.2–19.2% | **+147% to +164%** |

Under 3.2× CPU oversubscription, wall-clock medians moved about 150%. Instruction counts moved
0.035%. So: **gate CI on instruction counts, report wall clock without gating.**

Re-measured through `tak` itself, benchmarking `/bin/ls /usr` in a container, quiet versus
32 spinning cores:

| metric | quiet | 32-way contention |
|---|---|---|
| `instructions` | 536124 | **536124** — byte-identical, 8/8 runs |
| `wall_min_ms` | 0.66 | 0.66 – 1.17 (**77% swing**) |

`tests/counters.rs` asserts that determinism on every CI run, and skips cleanly where valgrind
is unavailable.

Corollary that cost me an afternoon to learn: syscall count and max RSS are *not* deterministic
— they move with thread scheduling. Only instruction counts support a tight gate.

**Second premise:** the data belongs in the repository. `tak` stores results in
`refs/notes/tak`, one JSON object per line, merged with git's `cat_sort_uniq` strategy so
concurrent CI writers never conflict. No database, no account, no service.

This has a genuinely nice property. The notes tree is keyed by commit SHA as *path names*, so
it does not reference the annotated commits at all — which means a single shallow fetch of one
ref returns the entire history without cloning the repository:

```console
$ git fetch --depth 1 origin '+refs/notes/tak:refs/notes/tak'
fetch of ONLY refs/notes/tak --depth 1:  36ms
refs present:                            1
project commit objects fetched:          0
notes cover:                             100 commits
on-disk size:                            124K
```

100 commits × 6 benchmarks packed to 124K. Ten thousand commits projects to roughly 6 MB.

## Status

Measuring and storing work. Nothing reads the data back yet.

- [x] `tak run` — wall clock (min / p50 / mean / max), no shell
- [x] instruction counts via cachegrind, degrading to timing-only where absent
- [x] `refs/notes/tak` storage — `run --record`, `push`, `history`, `init`
- [x] concurrent CI writers merge via `cat_sort_uniq`
- [x] `tak backfill` — benchmark published release binaries to bootstrap history
- [x] asset selection via `crates/asset-picker`, extracted from mise
- [ ] `tak compare <ref>` — interleaved A/B against another revision
- [ ] `bench.toml`
- [ ] PR reporting
- [ ] change-point detection instead of thresholds

Releases are automated with release-plz and publish to crates.io via trusted publishing —
see [RELEASING.md](RELEASING.md). The crate is `tak-cli`; the binary is `tak`.

## Counters on macOS and Windows

There is no usable cachegrind on Apple Silicon and none on Windows, so `tak run`
there records timing only and says so. Run it in a container to get the
deterministic metric on any host:

```sh
docker build -f docker/Dockerfile -t tak .
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/w" -w /w \
  tak run --bench startup -- ./mycli --help
```

The build context is the repository root, not `docker/`, because the build stage copies
`Cargo.toml` and `src/`. And `--user` is not decoration: `tak` spawns the thing you are
measuring, so the benchmark subject inherits the container's privileges over the mounted
working tree.

The CI gate lives on the Linux job regardless, so this only matters locally.

## Prior art this steals from

- [hyperfine](https://github.com/sharkdp/hyperfine) — the UX everyone knows. Its own
  [2.0 roadmap](https://github.com/sharkdp/hyperfine/issues/788) asks for most of what's above.
- [poop](https://github.com/andrewrk/poop) — hardware counters, no shell, first command as reference.
- [mise](https://github.com/jdx/mise) — `crates/asset-picker` is mise's release-asset
  selection logic extracted verbatim, tests and all. Picking the wrong asset yields a binary
  that runs and reports numbers that look real, so a naive heuristic here is worse than none.
- [git-appraise](https://github.com/google/git-appraise) — one JSON object per line in git notes,
  merged with `cat_sort_uniq`. The storage design is lifted from here wholesale.
- [git-perf](https://github.com/kaihowl/git-perf) — independently arrived at git-notes storage
  for performance data. Validates the approach; does not measure anything itself.
- [Chronologer](https://github.com/dandavison/chronologer) — benchmarking backwards across git
  history. Best idea in the field, sitting unused.
- [Nyrkiö](https://nyrkio.com) / [Hunter](https://github.com/datastax-labs/hunter) — E-divisive
  change point detection, proven at DataStax, TigerBeetle, Confluent.

## License

MIT
