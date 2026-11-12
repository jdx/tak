# Contributing

Use the mise tasks documented in `AGENTS.md` for development and validation.

## Report a bug

Bug reports, in Issues or Discussions, are only for problems you ran into while
using tak. Say what you were trying to do, what you ran, the relevant config,
and what happened. Reports found by reading the source or by having an agent
search the code will be closed.

## mbx build cache

mise wraps `cargo` with [mbx](https://mr-boxington.jdx.dev), so compiled work is
shared across checkouts. `mise run` tasks and `mise exec -- cargo …` use the
wrapper; plain `cargo` does too once mise is [activated in your
shell](https://mise.jdx.dev/getting-started.html#activate-mise). Builds that set
`MBX_DISABLE=1`, including CI and the `perf:build` task, skip the cache.
