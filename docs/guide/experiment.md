# The experiment

::: danger This is not a recommendation
tak is an unsupported experiment. It may be renamed, rewritten, or deleted. For real command
benchmarking, use [hyperfine](https://github.com/sharkdp/hyperfine); for CI benchmark tracking,
use a maintained tool such as [Bencher](https://bencher.dev) or
[CodSpeed](https://codspeed.io).
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
| timing | `wall_min_ms`, `wall_p50_ms`, `wall_mean_ms`, `wall_max_ms` | never |

Syscall counts and peak RSS are not deterministic enough for a tight threshold because thread
scheduling changes them.

## Why the minimum

Contention is one-sided: a busy machine can only make a run slower. A command that sometimes
consults the network can only retire more instructions. The floor is therefore the robust
estimator for both timing and instruction counts.

## Why runner classes matter

Moving between machine or toolchain classes shifts absolute measurements enough to resemble a
regression. Every series must be partitioned by `runner`; otherwise a threshold compares
unrelated populations.

Read the [repository README](https://github.com/jdx/tak#readme) for the full measurements and
prior art.
