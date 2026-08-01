# Adopt tak in a project

tak is most useful as a small loop rather than as a one-off timer:

1. declare repeatable benchmarks in `tak.toml`;
2. record the tip of every push to the main branch;
3. compare a pull request with the commit it branched from; and
4. fail only when an instruction count crosses the configured gate.

This is the pattern used by
[mise](https://github.com/jdx/mise/blob/main/tak.toml) and
[aube](https://github.com/jdx/aube/blob/main/tak.toml). Both projects are still early tak
adopters, not compatibility promises. Their checked-in files are useful examples, but tak is
pre-v1 and the details may change.

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
offline mode when the subject provides one. Prepare caches and stores before `tak run`, outside
the measured command.

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
          # Kept only for the trusted fetch below, before project code runs.
          persist-credentials: true

      - name: Fetch the comparison inputs
        id: base
        env:
          BASE_REF: ${{ github.base_ref }}
        run: |
          git fetch --quiet origin "+$BASE_REF:refs/remotes/origin/$BASE_REF"
          git fetch --quiet --depth 1 origin '+refs/notes/tak:refs/notes/tak'
          base=$(git merge-base "origin/$BASE_REF" HEAD)
          echo "sha=$base" >> "$GITHUB_OUTPUT"

      # Remove the checkout token before any action reads PR-controlled files
      # or any project build or benchmark command executes.
      - name: Remove checkout credentials
        run: git config --local --unset-all http.https://github.com/.extraheader

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
        run: |
          set +e
          tak compare "$BASE_SHA" > /tmp/tak-report.md
          status=$?
          set -e
          cat /tmp/tak-report.md
          cat /tmp/tak-report.md >> "$GITHUB_STEP_SUMMARY"
          if grep -Fq '**Nothing was compared' /tmp/tak-report.md; then
            echo "::error::no comparable baseline was found"
            status=1
          fi
          exit "$status"
```

Checking out the pull request's head SHA avoids measuring GitHub's synthetic merge commit.
Using the merge base avoids attributing unrelated changes that landed on main after the branch
was created to the pull request.

The example fetches the base branch and notes while its read-only checkout token is available,
then removes that credential before mise or any project command runs. `tak compare` falls back
to the prefetched local notes when its unauthenticated refresh fails. This keeps private
repositories readable without exposing their token to pull-request-controlled code.

An empty comparison is not a passing gate: it can mean that the base was never recorded or
that the runner classes differ. The explicit report check fails either case. If the workflow
also posts a sticky pull-request comment, keep the write token in a separate reporting job
that checks out no code and executes nothing from the pull request. Pass the Markdown report
and exit status to it as an artifact. mise's
[pull-request workflow](https://github.com/jdx/mise/blob/main/.github/workflows/perf-pr.yml)
shows that separation.

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

## Check the rollout

Before treating the comparison as a required check:

- `tak doctor` reports Valgrind and the runner class you intended;
- every measured command succeeds with its network pointed at a dead port;
- setup and cache warming happen before `tak run`;
- only main push tips are pushed into `refs/notes/tak`;
- main and pull requests use the same build inputs and runner class; and
- only instruction counts gate CI; timing remains report-only.

Continue with [benchmark configuration](/guide/configuration) for every setting or
[CI and git notes](/guide/ci) for the storage plumbing.
