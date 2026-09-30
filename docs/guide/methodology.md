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
| undecided | `alloc_blocks`, `alloc_bytes`, `alloc_peak_bytes` | not yet |
| timing | `wall_min_ms`, `wall_p50_ms`, `wall_mean_ms`, `wall_max_ms`, `wall_stddev_ms` | never |

Syscall counts and peak RSS are not deterministic enough for a tight threshold because thread
scheduling changes them.

## Heap allocations

Instruction counts can miss a change that allocates much more memory for little extra work.
With [`allocations = true`](/guide/configuration#counting-heap-allocations), tak also runs a
subject under valgrind's DHAT, which counts every heap allocation it intercepts. That is the
same kind of instrumentation as cachegrind's, so the counts might be as reproducible as
instruction counts. tak records and reports them, and doesn't gate on them until that has been
measured across more programs than these.

Each program below ran 20 times under DHAT and 20 times under cachegrind, on a 32-core Linux
host running valgrind 3.24.0 in a container. "Quiet" means no load was added, though other
work was running on the host. "Contended" means 32 `stress-ng --cpu` workers in the same
container. The figures are coefficients of variation.

| program | totals (`alloc_blocks`, `alloc_bytes`), quiet / contended | peak (`alloc_peak_bytes`), quiet / contended | peak minimum, contended vs quiet | `instructions`, quiet / contended |
|---|---|---|---|---|
| `tak --help` | 0 / 0 | 0 / 0 | 0 | 0 / 0 |
| `git status` in a 200-file repository | 26.6%, see below / 0 | 1.19%, see below / 0 | 0 | 0 / 0 |
| `git grep --threads=8` | 0 / 0 | 0.010% / 0.032% | +0.009% | 0.12% / 0.49% |
| Python, 4 threads allocating | 0 / 0 | 0 / 0.009% | 0 | 0.059% / 0.081% |
| Rust, 8 threads allocating at once | 0 / 0 | 0.51% / 1.07% | −0.72% | 0.003% / 0.003% |

`git status` allocated 1,140 blocks on its first run and 496 on each of the other 39, because
the first run refreshed the repository's index. That is a command doing different work, not
the metric varying, and it is what tak's spread warning is for. The minimum was the same in
both conditions. In a real run the timed samples come first, so the refresh happens before the
DHAT runs.

Apart from that first `git status` run, the totals repeated exactly for every program, threaded
ones included, under contention too. The peak didn't. Valgrind runs one thread at a time and decides itself when to switch, so how
much a threaded program has live at once depends on that schedule. The 8-thread program's
peak ranged from 26,526 to 27,198 bytes, 2.5% apart. Its minimum was *lower* under
contention, so for the peak the floor is not the one-sided estimator it is for time and
instructions.

That suggests the totals could join the deterministic tier and the peak should stay out of
it. Five programs are not enough to establish that, so for now none of them gates.

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

A `check` step verifies each timed sample's result, also untimed, directly after it. A
failure that only happens under the timing conditions, such as a race, is seen in the
samples that were timed rather than in separate verification runs. The terminal summary
shows how many samples passed next to the timing statistics, so a program that is fast
because it sometimes does the work wrong can't post that time there without also posting
its failures. It doesn't say which samples failed, so it can't tell you whether the reported
minimum came from one of them. `--export-json` can: it pairs each verdict with its sample's
time.

Git notes don't carry the verdicts. They fit neither of the rules recorded metrics are read
by: `tak compare` keeps each metric's minimum and treats lower as better. Stored alone, the
timings of a run with a failed check would look like those of a run that passed. So when
any check fails, `--record` writes nothing and the run exits non-zero, just as it does when a
subject is dropped. Only the run's own verdicts can vouch for its timings: an earlier clean
run can't rule out an intermittent failure in this one.

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
