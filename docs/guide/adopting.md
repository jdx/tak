# Adopt tak in a project

tak is most useful as a small loop rather than as a one-off timer:

1. declare repeatable benchmarks in `tak.toml`;
2. record the tip of every push to the main branch;
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

Pin the action to the full commit SHA of a release you have reviewed; the tags above are for
readability. The sections below show the same workflows without the action, for projects that
cannot use it or need to change what it does.

## Record the main branch

The main-branch workflow owns the history. It measures the tip of each push after it lands,
appends the result to `refs/notes/tak`, and pushes that ref:

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
```

Keep one runner class for the series and serialise writers. `tak push` retries by fetching and
merging if another writer wins the race, but serialisation avoids unnecessary retries. Do not
cancel an in-progress main run: that would leave a hole in the push-tip history. A push that
contains multiple commits records only its final commit; use one commit per push if every
intermediate commit must have a measurement.

The hosted runner label alone does not capture every input. If its image, compiler, standard
library, build profile, or CPU class changes, update `[runner].class` to start a new series.

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
          # tak.toml comes from the pull request, which could otherwise turn on
          # `accept_trailers` and accept its own regression with a trailer.
          TAK_ACCEPT_TRAILERS: "0"
        run: |
          set +e
          tak compare "$BASE_SHA" > /tmp/tak-report.md
          status=$?
          set -e
          cat /tmp/tak-report.md
          cat /tmp/tak-report.md >> "$GITHUB_STEP_SUMMARY"
          # Older tak releases exit 0 when nothing was compared; this keeps the
          # gate failing on them. Drop it when passing --allow-empty. Anchored
          # to the line start, where tak writes it: the report also echoes
          # commit-message text, which must not be able to fail the check.
          if grep -q '^\*\*Nothing was compared' /tmp/tak-report.md; then
            echo "::error::no comparable baseline was found"
            status=1
          fi
          exit "$status"
```

Checking out the pull request's head SHA avoids measuring GitHub's synthetic merge commit.
Using the merge base avoids attributing unrelated changes that landed on main after the branch
was created to the pull request.

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
report and exit 0, which is why the example also checks the report for `**Nothing was compared`:
the gate then fails whichever version the workflow pins.

Two situations produce an empty comparison legitimately: the first pull request after adopting
tak, whose merge base predates any main-branch measurement, and a runner-class migration, where
every base series is on the old class. Pass `--allow-empty` for those pull requests, and remove
the report check while it is passed, or the check still fails the job. Restore both once main has
measurements on the current class. Left in place, `--allow-empty` lets a workflow that has
stopped recording pass without comparing anything. A regression still fails under
`--allow-empty`; only `--no-gate` reports without ever failing.

If the workflow also posts a sticky pull-request comment, keep the write token in a separate
reporting job that checks out no code and executes nothing from the pull request. Pass the
Markdown report and exit status to it as an artifact. mise's
[pull-request workflow](https://github.com/jdx/mise/blob/main/.github/workflows/perf-pr.yml)
shows that separation.

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

`tak compare` reads `tak.toml` from the pull request's checkout, so a pull request can turn
trailers on for itself. Set `TAK_ACCEPT_TRAILERS: "0"` in the environment of the job that
compares, as the workflow above does. The environment takes precedence over the file. With
[jdx/tak-action](#use-the-github-action), put it in the compare job's `env:` so that it
reaches the action's steps. Whether the action can pass `--accept` depends on its release;
its README lists its inputs.

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

## Check the rollout

Before treating the comparison as a required check:

- `tak doctor` reports Valgrind and the runner class you intended;
- every measured command succeeds with its network pointed at a dead port;
- setup and cache warming happen before `tak run` or in an untimed `setup` or `prepare`;
- only main push tips are pushed into `refs/notes/tak`;
- main and pull requests use the same build inputs and runner class; and
- only instruction counts gate CI; timing remains report-only.

Continue with [benchmark configuration](/guide/configuration) for every setting or
[CI and git notes](/guide/ci) for the storage plumbing.
