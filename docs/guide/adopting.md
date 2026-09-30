# Adopt tak in a project

tak is most useful as a small loop rather than as a one-off timer:

1. declare repeatable benchmarks in `tak.toml`;
2. record the tip of every push to the main branch, and check what landed;
3. compare a pull request with the commit it branched from; and
4. fail only when an instruction count crosses the configured gate.

This is the pattern used by
[mise](https://github.com/jdx/mise/blob/main/tak.toml) and
[aube](https://github.com/jdx/aube/blob/main/tak.toml). They are useful examples of tak in real
CI. tak is pre-v1, so check which version each repository pins before copying its config.

## Choose work that can be measured

Start with two or three commands, not every command the program exposes:

- a **startup control** that does as little application work as possible;
- one or two **representative paths** whose cost matters to users; and
- a committed fixture large enough that the representative path is visible above startup.

When every benchmark moves together, the startup control points toward fixed process cost. If
only one representative path moves, the change is more likely inside that path.

The measured command must not depend on the network, the clock, a floating version, or mutable
machine state. Verify that claim rather than assuming it: point network configuration at a
dead port, run the benchmark, and confirm that it still succeeds with identical output. Use an
offline mode when the subject provides one. Prepare caches and stores outside the measured
command: before `tak run`, or in an untimed [`setup`](/guide/configuration#setting-up-once-before-measuring),
which runs once for each subject before sampling starts. When every sample needs a reset, such
as an empty `node_modules`, declare it as
[`prepare`](/guide/configuration#resetting-state-before-each-sample), which runs untimed before
each sample. When a command's result can come out wrong, such as fixers that may
race on the same files, declare a [`check`](/guide/configuration#checking-every-sample) that
verifies it after every timed sample. `--record` then writes nothing unless every check in
the run passed.

The comments in mise's [benchmark configuration](https://github.com/jdx/mise/blob/main/tak.toml)
and aube's [benchmark configuration](https://github.com/jdx/aube/blob/main/tak.toml) explain
why each command was included or rejected.

## Declare the benchmarks

For a compiled CLI, a first `tak.toml` might look like this:

```toml
[gate]
pct = 1.0

# Change this when the compiler, base image, or runner class changes. Numbers
# on either side of that change do not belong in the same series.
[runner]
class = "gha-linux-x64-rust1.90"

[bench.startup]
cmd = ["./target/release/mycli", "--help"]

[bench.resolve]
cmd = ["./target/release/mycli", "-C", "fixtures/medium", "resolve", "--offline"]
```

Pin tak itself alongside the compiler and other tools used to build the subject. Changing the
measuring instrument can create a step in the series that looks like a change in the subject.
Upgrade it deliberately in a commit that makes no other performance-affecting change.

Put the build and any preparation in the project's normal task runner so local and CI runs
invoke the same commands. With mise:

```toml
[tools]
tak = "0.0.5"

[tasks.perf]
run = [
  "cargo build --release",
  "tak run",
]

[tasks."perf:record"]
run = [
  "cargo build --release",
  "tak run --record",
]
```

Use the current tak version when adopting it; the pin above only illustrates the shape. Run
`mise run perf` locally before adding CI. `tak doctor` should report Valgrind and the intended
runner class on the machine that will produce the shared series.

## Use the GitHub Action

[jdx/tak-action](https://github.com/jdx/tak-action) is the recommended way to run the loop on
GitHub Actions. It is pre-v1 like tak, and its inputs and behavior may change between releases.
It installs a pinned tak release and Valgrind, and provides three modes that replace the
workflows written out by hand in the rest of this page:

- `record` measures each push to main and pushes `refs/notes/tak`;
- `compare` fetches the base branch and notes with a read-only token, removes the checkout's
  credentials, runs your build, compares with the merge base, and fails on an
  instruction-count regression or when nothing was compared; and
- `comment` runs in a separate `workflow_run` job that holds the write token, and posts the
  compare job's report as a sticky pull-request comment and a check run without checking out
  or executing anything from the pull request.

The main-branch job:

```yaml
jobs:
  record:
    runs-on: ubuntu-24.04
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: jdx/tak-action@v0.1.0
        with:
          mode: record
          version: 0.0.13
          run: |
            cargo build --release
            tak run --record
          token: ${{ secrets.GITHUB_TOKEN }}
```

The pull-request job, with only `contents: read`:

```yaml
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false
      - uses: jdx/tak-action@v0.1.0
        with:
          mode: compare
          version: 0.0.13
          run: |
            cargo build --release
            tak run --record
```

`run` executes after the credentials are removed, so the whole build belongs there. When the
build needs another action first, such as `jdx/mise-action`, run the action with
`mode: prepare` before it and `mode: compare` after it. If `mise.toml` already pins tak, set
`install: false` so the action uses that tak rather than a second copy. The action's
[README](https://github.com/jdx/tak-action#readme) has the complete workflows, including the
`workflow_run` reporting job, every input, and the security model and limitations.

None of the three modes runs `tak detect`, which checks whether a push to main introduced an
instruction-count step. `tak detect` needs a tak release that includes it, and tak 0.0.13,
pinned in the examples above, does not. To add the check to the main-branch job, raise
`version: 0.0.13` to such a release (`version: X.Y.Z`), give the checkout `fetch-depth: 0`,
and add a later step that runs `tak detect` with that tak on `PATH`, as described under
[Record the main branch](#record-the-main-branch). The manual workflow there already includes
the step.

Pin the action to the full commit SHA of a release you have reviewed; the tags above are for
readability. The sections below show the same workflows without the action, for projects that
cannot use it or need to change what it does.

## Record the main branch

The main-branch workflow owns the history. It measures the tip of each push after it lands,
appends the result to `refs/notes/tak`, pushes that ref, and then checks whether the push
introduced an instruction-count step:

```yaml
name: perf

on:
  push:
    branches: [main]
  workflow_dispatch:

permissions: {}

concurrency:
  group: perf
  cancel-in-progress: false

jobs:
  measure:
    runs-on: ubuntu-24.04
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          # tak detect walks the commits between recorded points.
          fetch-depth: 0
          persist-credentials: false

      - uses: jdx/mise-action@dad1bfd3df957f44999b559dd69dc1671cb4e9ea # v4.2.1

      - name: Install Valgrind
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends valgrind

      - name: Measure and record
        run: mise run perf:record

      - name: Push measurements
        if: github.ref == 'refs/heads/main'
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          REPOSITORY: ${{ github.repository }}
        run: |
          git remote set-url origin "https://x-access-token:${GITHUB_TOKEN}@github.com/${REPOSITORY}.git"
          tak push

      - name: Summary
        run: tak history >> "$GITHUB_STEP_SUMMARY"

      - name: Detect regressions that landed
        run: |
          set -o pipefail
          tak detect | tee -a "$GITHUB_STEP_SUMMARY"
```

Keep one runner class for the series and serialise writers. `tak push` retries by fetching and
merging if another writer wins the race, but serialisation avoids unnecessary retries. Do not
cancel an in-progress main run: that would leave a hole in the push-tip history. A push that
contains multiple commits records only its final commit. Use one commit per push if every
intermediate commit must have a measurement, or fill the gap afterwards with
[`tak backfill --commits`](#backfill-commits-by-building-them).

`tak detect` needs a tak release that includes it. The `tak = "0.0.5"` pin in the mise example
above does not, so pin such a release (`tak = "X.Y.Z"`) before adding the step.

`tak detect` runs after `tak push` so the measurement is published even when the check fails.
It fails when the step onto the commit being measured exceeds the gate, including a step
spread over earlier pushes that were not recorded. The run for the next push passes again.
Older steps and sub-threshold drift are listed in the job summary without failing. A failed
run on the main branch is the notification: GitHub marks the commit with a failed check and,
subject to their notification settings, notifies whoever triggered the run, which for a push
is the person who pushed or merged it. A team that wants more can add an `if: failure()` step
that opens an issue or posts to chat. See
[CI and git notes](/guide/ci#detect-regressions-that-landed) for exactly what is reported.

`tak detect` also fails when it has nothing to compare: the commit has no instruction counts,
or none of its series has an earlier recorded point. On the first recording, and on the first
run after changing `[runner].class`, that is expected. Either seed the history first, by
adding the recording workflow in one push and the `tak detect` step in a later one, or run
that one push with `tak detect --allow-empty` and remove the flag afterwards. Leaving
`--allow-empty` in place means a checkout without history or a recording that stopped
producing instruction counts also passes.

A regression accepted on its pull request through a label is not recorded anywhere
`tak detect` can see, so the main-branch check fails once on the commit that merged it. To
avoid that, have the merged commit carry a `Tak-Accept:` trailer and set
`TAK_ACCEPT_TRAILERS: "1"` on the detect step, or re-run that one check with
`tak detect --accept BENCH`. See
[accepted steps on main](/guide/ci#accepted-steps-on-main).

`fetch-depth: 0` gives the walk the commits between recorded points. A bounded depth works if
it reaches back past the oldest of the 20 recorded commits the check examines by default.

The hosted runner label alone does not capture every input. If its image, compiler, standard
library, build profile, or CPU class changes, update `[runner].class` to start a new series,
and pass `--allow-empty` to `tak detect` for the push that makes the change.

See mise's [main-branch workflow](https://github.com/jdx/mise/blob/main/.github/workflows/perf.yml)
for a pinned, cache-aware example.

## Gate pull requests

Collect some main-branch history before adding a gate. A first point has nothing to compare
against, and a short series does not show whether the chosen benchmark is actually stable.

The pull-request workflow measures the branch commit locally and compares it with the merge
base. It must not push the branch measurement into the main history:

```yaml
name: perf-pr

on:
  pull_request:
    types: [opened, synchronize, reopened]

permissions:
  contents: read

concurrency:
  group: ${{ github.workflow }}-${{ github.event.pull_request.number }}
  cancel-in-progress: true

jobs:
  compare:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          # Nothing is written to disk. The one step that needs the token
          # gets it through its environment, before project code runs.
          persist-credentials: false

      - name: Fetch the comparison inputs
        id: base
        env:
          BASE_REF: ${{ github.base_ref }}
          GITHUB_TOKEN: ${{ github.token }}
        run: |
          auth=$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 -w0)
          echo "::add-mask::$auth"
          export GIT_CONFIG_COUNT=1
          export GIT_CONFIG_KEY_0=http.https://github.com/.extraheader
          export GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $auth"
          git fetch --quiet origin "+$BASE_REF:refs/remotes/origin/$BASE_REF"
          git fetch --quiet --depth 1 origin '+refs/notes/tak:refs/notes/tak'
          base=$(git merge-base "origin/$BASE_REF" HEAD)
          echo "sha=$base" >> "$GITHUB_OUTPUT"

      - uses: jdx/mise-action@dad1bfd3df957f44999b559dd69dc1671cb4e9ea # v4.2.1

      - name: Install Valgrind
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends valgrind

      - name: Measure this pull request
        run: mise run perf:record

      - name: Compare and gate
        env:
          BASE_SHA: ${{ steps.base.outputs.sha }}
          # The gate comes from the base's tak.toml, so the pull request cannot
          # turn on `accept_trailers` itself. This keeps trailers off here even
          # if tak.toml turns them on for the main branch.
          TAK_ACCEPT_TRAILERS: "0"
        run: |
          set +e
          tak compare "$BASE_SHA" > /tmp/tak-report.md
          status=$?
          set -e
          cat /tmp/tak-report.md
          cat /tmp/tak-report.md >> "$GITHUB_STEP_SUMMARY"
          # Older tak releases exit 0 when nothing was compared; this keeps the
          # gate failing on them. Drop it when passing --allow-empty. Only the
          # first line, which tak writes before any name: the rest of the
          # report echoes text from the pull request, such as benchmark names
          # and commit trailers, which must not be able to fail the check.
          if head -n 1 /tmp/tak-report.md | grep -q '^\*\*Nothing was compared'; then
            echo "::error::no comparable baseline was found"
            status=1
          fi
          exit "$status"
```

Checking out the pull request's head SHA avoids measuring GitHub's synthetic merge commit.
Using the merge base avoids attributing unrelated changes that landed on main after the branch
was created to the pull request.

`tak compare` reads the gate from the merge base's `tak.toml`, not the pull request's, so a pull
request cannot loosen its own gate. A gate change takes effect once it is merged, and the report
says so on the pull request that makes it. This needs the merge base's commit and tree in the
clone, which `fetch-depth: 0` and the base-branch fetch above provide. See
[where the gate comes from](/guide/ci#where-the-gate-comes-from).

The example passes the read-only token to git through the environment of the one step that
fetches the base branch and notes, so it is never written to `.git/config` and is out of reach
once mise or any project command runs. Do not keep `persist-credentials: true` and delete the
credential afterwards: since actions/checkout v6 the token is stored in a separate file under
`$RUNNER_TEMP`, included from `.git/config`, and
`git config --local --unset-all http.https://github.com/.extraheader` neither finds nor removes
it. `tak compare` falls back
to the prefetched local notes when its unauthenticated refresh fails. This keeps private
repositories readable without exposing their token to pull-request-controlled code.

An empty comparison is not a passing gate. When no series was measured on both the merge base
and the pull request, because the base was never recorded or the runner classes differ,
`tak compare` prints the full report and then exits non-zero. Older releases print the same
report and exit 0, which is why the example also checks whether the report's first line starts
with `**Nothing was compared`. The gate then fails whichever version the workflow pins.

Two situations produce an empty comparison legitimately: the first pull request after adopting
tak, whose merge base predates any main-branch measurement, and a runner-class migration, where
every base series is on the old class. Pass `--allow-empty` for those pull requests, and remove
the report check while it is passed, or the check still fails the job. Restore both once main has
measurements on the current class. Left in place, `--allow-empty` lets a workflow that has
stopped recording pass without comparing anything. A regression still fails under
`--allow-empty`; only `--no-gate` reports without failing on the comparison, and errors still
fail under it.

If the workflow also posts a sticky pull-request comment, keep the write token in a separate
reporting job that checks out no code and executes nothing from the pull request. Pass the
Markdown report and exit status to it as an artifact. mise's
[pull-request workflow](https://github.com/jdx/mise/blob/main/.github/workflows/perf-pr.yml)
shows that separation, and the `comment` mode of [jdx/tak-action](#use-the-github-action) is
such a reporting job.

### When a regression is intentional

A change that makes a benchmark more expensive on purpose should be accepted for the affected
benchmark. Do not weaken the gate for every benchmark. Pass the benchmark name to the compare
step:

```sh
tak compare "$BASE_SHA" --accept startup
```

The regression in `startup` is reported as accepted, and every other benchmark still gates.
In CI, drive `--accept` from a pull-request label that only maintainers can apply. Then the
pull request's author cannot waive their own gate.
[Accepting an intentional regression](/guide/ci#accept-from-a-pull-request-label) shows a
workflow step that does this.

A `Tak-Accept: startup` commit trailer can do the same, but only after the project opts in
with `[gate] accept_trailers = true`. Trailers are written by the change being gated, so they
are ignored by default. If the project opts in and squash-merges pull requests, keep the
trailer in the final paragraph of the squashed commit message.

`tak compare` reads `accept_trailers` from the merge base's `tak.toml`, so a pull request cannot
turn trailers on for itself. If `tak.toml` turns them on for the main branch, they are on for
pull requests too. Set `TAK_ACCEPT_TRAILERS: "0"` in the environment of the job that compares,
as the workflow above does, to keep them off there. The environment takes precedence over the
file. With
[jdx/tak-action](#use-the-github-action), put it in the compare job's `env:` so that it
reaches the action's steps. The action's `accept` input passes each line as its own
`--accept`. It needs a tak release that includes `--accept`, and tak 0.0.13, pinned in the
examples above, does not. The action's
[README](https://github.com/jdx/tak-action#accepting-an-intentional-regression) shows a
label-driven step.

An acceptance never makes an empty comparison pass. When nothing was measured on both sides,
`tak compare` still fails unless `--allow-empty` or `--no-gate` is given.

## Backfill published releases

A new adopter can seed history with `tak backfill` instead of rebuilding many historical
commits. This works when releases contain executable assets and one command is compatible
across the releases being measured:

```sh
git fetch --force --tags origin
tak backfill --bench release-startup --limit 20 -- --help
if [ -z "$(git notes --ref=tak list)" ]; then
  echo "backfill recorded no releases" >&2
  exit 1
fi
tak push
```

`tak backfill` resolves each release tag to the commit that produced it. Run it from a full
clone or fetch all release tags first, as above. Missing tags are skipped, so checking the
notes before pushing prevents a shallow checkout from publishing no release history without
warning.

Keep backfilled release artifacts in a separate benchmark series from binaries built by the
ongoing CI workflow. Different build pipelines can produce different instruction counts even
for the same source commit.

Only backfill release assets that you trust: tak downloads and executes them. A workflow
should run them on an ephemeral runner with no credentials, restrict network egress, and pass
only the generated notes artifact to a separate publishing job. If that isolation is not
available, limit backfill to binaries produced by a release pipeline you trust. Aube's
[backfill workflow](https://github.com/jdx/aube/blob/main/.github/workflows/perf-backfill.yml)
shows the separate measurement and publishing jobs; it does not provide an execution sandbox.
`env_deny` only changes the binary's direct environment; it cannot stop hostile code from
inspecting other same-user processes or files and is not a substitute for that isolation.

## Backfill commits by building them

`tak backfill --commits RANGE` builds and measures past commits instead of downloading
releases. Use it to seed a baseline before the first pull-request gate, or to fill in the
commits of a multi-commit push that recorded only its tip. Declare the build in
[`[build]`](/guide/configuration#building-past-commits):

```toml
[build]
cmd = ["cargo", "build", "--release", "--locked"]

[bench.startup]
cmd = ["./target/release/mycli", "--help"]
```

Check what would happen first. A dry run lists the commits and builds nothing:

```sh
tak backfill --commits main~20..main --dry-run
tak backfill --commits main~20..main
tak push
```

For each first-parent commit in the range, newest first, tak:

1. checks the commit out into a detached `git worktree` under a private temporary directory,
   with the repository's hooks turned off;
2. runs `[build]` there;
3. runs the benchmarks declared in the **current** `tak.toml`, anchored at the same place in
   that checkout, so `./target/release/mycli` is the binary built from that commit; and
4. appends the results to that commit's note, with the committer date from the commit
   object as `ts`, including dates before 1970.

The range is whatever `git rev-list` accepts as one argument. `A..B` includes `B` and excludes
`A`, so `main~20..main` is the last twenty commits. Merged branches are not walked, because
their commits were never points on the main branch's timeline.

A subject that already has a record on a commit for this runner class is not measured there
again. A commit where every subject has one is skipped without being built. When a subject is
added to a benchmark, the next backfill measures only that subject on the commits recorded
before it existed. Pass `--force` to measure everything again, which adds a second record
rather than replacing the first. tak fetches `refs/notes/tak` from `origin` first, so commits
that CI already recorded are skipped rather than measured twice.

`--limit` (default 20) caps how many commits are built, so running the same command again
continues where the last run stopped. `--bench NAME` limits the backfill to one declared
benchmark, and `--runs N` overrides the file's run count.

Each run of a commit is recorded whole or not at all, and the rest of the range continues
whatever happens to one commit. A commit is left unrecorded and reported in two cases:

- its build fails, or a `[build].dir` is missing from its tree or leads out of it;
- it builds, but measuring a benchmark fails: it errors, a subject is dropped, a check fails,
  an instruction count that was asked for fails with Valgrind present, or a path the current
  `tak.toml` names is missing or leads out of the checkout.

In either case nothing from that run is written for the commit, not even the benchmarks that
did measure. Old commits that no longer build or run are normal.

tak remembers each failure in git's directory (`.git/tak/backfill-build-failed`), and later
runs pass over it. Without that, a commit that can never be recorded would be tried first on
every run, and with `--limit` the backfill would never reach older commits.

- A build failure covers the whole commit. It is tried again when the runner class or `[build]`
  (its `cmd`, `dir` or `env`) changes.
- A measurement failure covers only the benchmark that failed. The next run measures the
  commit's other benchmarks without it, and a run with `--bench` for another benchmark is not
  affected. It is tried again when anything in `tak.toml` changes, since editing a benchmark is
  how one gets fixed. `--runs` is not part of it: a benchmark that fails at five runs fails at
  ten.

`--force` retries both kinds, and so does deleting the file. The file is local to the clone
and never pushed. `--dry-run` shows a commit as `build failed before` or `measure failed
before` when nothing is left to try, and as `would build (…; B failed before)` when only some
benchmarks are. A measurement that failed for a reason that does not repeat, such as a flaky
`check`, is passed over too until one of those happens. The command fails only when the range
ends with no record at all.

Build output goes to a file in the temporary directory, and a failed build shows its last 20
lines. `[build].dir`, and a benchmark's `dir` or program inside the checkout, must resolve
inside the checkout once symlinks are followed. The old tree's own code runs during
measurement, so they are checked again after every `setup` of a benchmark has run and before
every sample, after its `prepare`. A commit with a symlink at one of those paths that points
elsewhere is reported and not recorded. The check before each sample is untimed and took
about 17µs on a Linux host.

The checkouts are removed when tak finishes, fails, or is stopped with Ctrl-C, SIGTERM or
SIGHUP, and nothing is recorded or remembered for the commit that was in progress. On Unix,
during a backfill, the build and each measured command run in a process group of their own,
and tak kills the one running before it removes the checkout. That covers a terminal's Ctrl-C
and a signal sent to tak alone, as when CI cancels a job, and it never signals anything else,
such as the rest of a `tak backfill … | tee log` pipeline. On Windows, and if a subject's
`version_cmd` is running when the signal arrives, an interrupted run can leave its checkout in
the temporary directory. Once that directory is deleted, the next backfill runs
`git worktree prune`, which clears git's record of it.

The benchmarks come from the current `tak.toml`, not each commit's own copy, so a series keeps
measuring the same thing when someone edits a benchmark. The cost is that an old tree may lack
a fixture or path the current file names. That commit is then reported and left unrecorded.
To measure old commits against a fixture they did not contain, create it in a `setup`, or point
`dir` at an absolute path outside the repository, for example with a
[template](/guide/configuration#templates) that reads an environment variable.

Backfilled numbers are only comparable with the ones CI records if they are produced the same
way. That means the same runner class, compiler, build profile, flags and lockfile policy as
the main-branch workflow. Run the backfill on the runner class that records main, with the
`[runner].class` it uses, and build with the same command the main-branch workflow runs.
If `[build]` differs from how CI builds, record the backfill under a different benchmark or
runner class rather than mixing the two into one series. A step between the backfilled points
and the first CI point is a sign they were not built identically.

Every commit is built from a clean checkout. That is slow for a compiled project unless the
build tool caches outside the tree, for example with `sccache`. Submodules are not initialised
unless the build does it. As with release backfill, tak runs whatever those commits build, so
backfill only history you trust, and run it without credentials the build does not need.

## Check the rollout

Before treating the comparison as a required check:

- `tak doctor` reports Valgrind and the runner class you intended;
- every measured command succeeds with its network pointed at a dead port;
- setup and cache warming happen before `tak run` or in an untimed `setup` or `prepare`;
- only main push tips are pushed into `refs/notes/tak`;
- the main-branch workflow runs `tak detect` after `tak push`, from a checkout with history;
- main and pull requests use the same build inputs and runner class; and
- only instruction counts gate CI; timing remains report-only.

Continue with [benchmark configuration](/guide/configuration) for every setting or
[CI and git notes](/guide/ci) for the storage plumbing.
