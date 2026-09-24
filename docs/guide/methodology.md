# Methodology

::: warning Pre-v1 software
tak is pre-v1. Its interfaces and behavior are not finalized and may change incompatibly
between releases. If you need a stable command benchmark, use
[hyperfine](https://github.com/sharkdp/hyperfine); for CI benchmark tracking, use
[Bencher](https://bencher.dev) or
[CodSpeed](https://codspeed.io).

These docs are currently AI slop and have not been fully reviewed. They will be reviewed and
finished later.
:::

tak asks whether retired instruction counts can make small CLI regressions visible in shared
CI, where wall-clock time is dominated by contention.

## The premise

On a shared runner, wall time has a noise floor in roughly the same range as the regressions
people want to catch. Thresholds either cry wolf or miss changes, and eventually get ignored.

In the measurements that motivated tak, cachegrind instruction counts varied by about
0.008–0.027% on a quiet host and 0.011–0.021% under heavy CPU contention. Median instruction
counts moved no more than 0.035% between those conditions while wall-clock medians moved by
roughly 150%.

That leads to one narrow rule:

> Gate on instruction counts. Report wall time without gating on it.

## Two tiers of metrics

| tier | metrics | may gate CI? |
|---|---|---|
| deterministic | `instructions` | yes |
| timing | `wall_min_ms`, `wall_p50_ms`, `wall_mean_ms`, `wall_max_ms`, `wall_stddev_ms` | never |

Syscall counts and peak RSS are not deterministic enough for a tight threshold because thread
scheduling changes them.

## Why the minimum

Contention is one-sided: a busy machine can only make a run slower. A command that sometimes
consults the network can only retire more instructions. The floor is therefore the robust
estimator for both timing and instruction counts.

## Comparing programs

When one benchmark compares several programs, the order samples are taken in matters as much as
how many there are. Running every sample of one program and then every sample of the next puts
anything that drifts during the run on whichever program was running at the time. That drift
might be contention on a shared host, thermal throttling or a cache filling up. The difference
then looks like one program being slower than another.

tak takes one sample of each program per round and shuffles the order every round. Drift is
then spread across all the programs instead of landing on one of them. Shuffling also stops a
program from always running first, or always running right after the same other program and
inheriting whatever state it left behind. A program with fewer samples takes part in evenly
spaced rounds rather than only the early ones.

A `prepare` step can reset state before each sample without being timed. It runs directly
before its sample, so the reset is as fresh for the last round as for the first.

The seed makes an order repeatable. It does not make the timings repeatable: wall-clock
comparisons between programs are still subject to the noise described above, and are never
gated.

## Why runner classes matter

Moving between machine or toolchain classes shifts absolute measurements enough to resemble a
regression. Every series must be partitioned by `runner`; otherwise a threshold compares
unrelated populations.

## Prior art

tak borrows heavily from:

- [hyperfine](https://github.com/sharkdp/hyperfine) and
  [poop](https://github.com/andrewrk/poop) for command benchmarking
- [git-appraise](https://github.com/google/git-appraise) and
  [git-perf](https://github.com/kaihowl/git-perf) for git-notes storage
- [Chronologer](https://github.com/dandavison/chronologer) for benchmarking backward through
  history
- [Nyrkiö](https://nyrkio.com) and
  [Hunter](https://github.com/datastax-labs/hunter) for change-point detection

The release-asset selection in `crates/asset-picker` was extracted from
[mise](https://github.com/jdx/mise).
