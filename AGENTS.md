# tak Development Guide

## mbx build cache

Mise installs mbx 1.3 and activates its transparent Cargo wrapper, so
compilation-heavy mise tasks use ordinary `cargo` commands. If the wrapper fails
or creates a development papercut, rerun the exact equivalent command from
`CONTRIBUTING.md` with `MBX_DISABLE=1`; this unblocks work without weakening the
check. If bypassed Cargo succeeds, surface the mismatch and recommend a
[mr-boxington Discussion](https://github.com/jdx/mr-boxington/discussions) with
the repository and commit, OS, `mbx --version`, `mbx doctor`, and both commands
and outputs. Redact secrets, absolute cache paths, remote URLs, namespaces, and
other sensitive or identifying details. Do not permanently disable the wrapper,
and do not post externally without user authorization.

tak measures how much work a command does by counting instructions, and stores the results in
the repository as git notes. Read the README before changing anything: the premise, the
measured numbers behind it, and the pre-v1 compatibility warning are all there.

## The invariant everything else serves

Metrics come in two tiers, and conflating them defeats the entire point of the project:

| tier | metrics | may gate CI? |
|---|---|---|
| **deterministic** | `instructions` | **yes** — ~0.02% run-to-run, ~0.035% across wildly different machine load |
| **timing** | `wall_min_ms`, `wall_p50_ms`, `wall_mean_ms`, `wall_max_ms` | **never** — 4–20% CV on a quiet host, medians move ~150% under contention |

Syscall counts and peak RSS sit between the two (~1%) and are *not* deterministic — they move
with thread scheduling. Record and flag them; never gate on them at a tight threshold.

Two consequences that come up constantly:

- **Report the minimum, not the mean.** Contention is one-sided — a busy machine can only make
  a run slower, and a subject that consults the network can only retire *more* instructions.
  The floor is the robust estimator for both tiers.
- **Series must be partitioned on `runner`.** Moving between runner classes shifts absolute
  numbers enough to look like a regression. That is the documented failure mode of every
  threshold-based CI benchmark, and reproducing it here would be embarrassing.

## Build & test

Go through mise rather than calling cargo directly, so that a laptop and a CI runner invoke
the same commands — the same reason `tak.toml` exists for benchmarks.

```bash
mise run build        # cargo build
mise run test         # cargo test --all-targets, including tests/counters.rs
mise run lint         # fmt check + clippy, warnings as errors
mise run lint-fix     # auto-fix what can be auto-fixed
mise run ci           # build + test + lint, everything CI runs
```

`mise run lint:fmt` and `mise run lint:clippy` are separately runnable. `mise tasks` lists
everything.

**Instruction counting needs valgrind.** Without it `measure::instructions` returns
`Ok(None)` and every counter test skips — which is how the cachegrind call once shipped having
never executed once. `tests/counters.rs` exists to stop that recurring, so if you touch the
measurement path, run it somewhere valgrind exists:

```bash
sudo apt-get install -y valgrind
```

No usable valgrind exists on Apple Silicon or Windows. On those hosts, `mise run docker` builds
an image that carries it:

```bash
mise run docker
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/w" -w /w tak run --bench startup -- ./mycli --help
```

Pass `--user`: tak spawns the subject under measurement, so it inherits the container's
privileges over your mounted checkout.

`tak doctor` reports what is actually available — git repo, valgrind, notes fetch, runner class.

## Layout

```
src/main.rs        clap CLI, one cmd_* fn per subcommand, runner_class(), now_rfc3339()
src/measure.rs     the two tiers: wall() and instructions()
src/notes.rs       refs/notes/tak storage; shells out to git for all network I/O
src/record.rs      the on-disk line format and its schema version
src/config.rs      tak.toml — declared benchmarks
src/backfill.rs    benchmark published release binaries to bootstrap history
crates/asset-picker/   release-asset selection, extracted from mise; published separately
tests/counters.rs  exercises the real cachegrind subprocess
```

`src/lib.rs` re-exports the modules so integration tests can reach them. That is the only
reason the library target exists — keep it that way rather than growing a public API.

## Conventions in this codebase

- **Errors:** `anyhow::Result` throughout, with `.context()` / `.with_context()` naming the
  thing that failed. `bail!` for domain errors. No `thiserror` yet.
- **No `tracing`, no `log`.** Results go to stdout with `println!`; diagnostics to stderr with
  `eprintln!`. This is a CLI whose entire output is a handful of numbers.
- **Never spawn a shell.** A shell adds its own startup cost and variance to every sample,
  which for a 10ms command is a large fraction of the measurement. String commands in
  `tak.toml` split on whitespace and nothing else — no quoting, no globs. Anything needing a
  pipe is a list whose first element is the interpreter.
- **`BTreeMap`, not `HashMap`,** wherever the contents are serialised or iterated. Key order
  has to be stable across writers and across runs.
- **Git is a subprocess, deliberately.** `actions/checkout` sets up auth via
  `http.extraheader`; users have credential helpers, SSH agents and corporate proxies.
  Reimplementing that is a trap. Local object access could move to `gix` without crossing this
  boundary — network operations should not.
- **Validate up front.** `tak.toml` is fully validated at parse time, and a multi-benchmark run
  measures everything before writing anything. A partial set left behind by a late failure
  looks exactly like a complete one.
- **Comments say *why*, not *what*.** Nearly every non-obvious line here carries the reasoning
  or the measurement that produced it, often naming the failure it prevents. Match that. A
  comment restating the code is worse than none.

### Touching the record format

`refs/notes/tak` is merged with git's `cat_sort_uniq` strategy, which is what lets concurrent
CI writers avoid conflicts without a custom merge driver. It dedupes on **exact bytes**, so:

- Two writers emitting the same measurement must produce byte-identical lines. Everything goes
  through `Record::to_line`, and map keys stay sorted.
- Any incompatible change to the shape bumps `SCHEMA_VERSION`. Readers skip lines from a
  version they do not understand, so a newer writer never breaks an older reader.
- A single malformed line must not render a commit's history unreadable — `parse_note` skips
  what it cannot parse.

## Dependency updates

- Use the lowest compatibility-significant specificity in `Cargo.toml` (for example, `"1"` for stable 1.x dependencies).
- When the existing manifest requirement accepts a routine dependency update, change only `Cargo.lock`.
- Keep lockfile updates focused and avoid unrelated transitive dependency churn.

## Conventional commits

`<type>(<scope>): <description>`, lowercase, imperative mood.

**Types:** `feat`, `fix`, `refactor`, `docs`, `style`, `perf`, `test`, `chore`, `ci`

Scopes are optional and mostly unused here; when one helps, use a subcommand (`run`,
`backfill`, `history`) or a subsystem (`notes`, `measure`, `config`, `changelog`).

Only `fix:` and `feat:` commits trigger a release — see the cadence guards in RELEASING.md.

## Releasing

Automated end to end; see [RELEASING.md](RELEASING.md). The parts that catch people out:

- Nobody merges the release PR by hand. `auto-merge-release.yml` does it daily, but only when
  the last tag is ≥7 days old *and* a `fix:`/`feat:` has landed since.
- Renaming `.github/workflows/release-plz.yml` breaks crates.io publishing until the trusted
  publisher is updated, because the OIDC claim names the workflow file.
- `tak-cli` takes the bare `vX.Y.Z` tag; other workspace crates get package-prefixed ones.
  `cliff.toml`'s `tag_pattern` matches both, and must keep doing so.

Published crates carry source and licence only — repository furniture is listed in
`Cargo.toml`'s `exclude`. Add new tooling files there.

## GitHub interactions

Opening pull requests and discussions is fine. Issues are disabled on this repository, as on
every jdx project — discussions are where that traffic goes.

PR titles follow the same conventional-commit format as commits. Do not prefix them with agent
or tool labels such as `[claude]` or `[codex]`.

When posting comments on PRs or discussions, note that the comment was AI-generated.

## Tone

The README opens with a warning that tak is pre-v1 and its interfaces, configuration, storage
format, and behavior are not finalized. Match that in anything user-facing — release notes,
docs, PR descriptions. No marketing language or implying production readiness. Call out
breaking changes plainly. `communique.toml` carries the same instruction for generated release
notes.
