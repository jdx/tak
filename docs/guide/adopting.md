# Adopt tak in a project

tak is most useful as a small loop rather than as a one-off timer:

1. declare repeatable benchmarks in `tak.toml`;
2. record every commit that lands on the main branch;
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

The main-branch workflow owns the history. It measures a commit after it lands, appends the
result to `refs/notes/tak`, and pushes that ref:

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
      - uses: actions/checkout@v7
        with:
          persist-credentials: false

      - uses: jdx/mise-action@v4

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

Pin actions by commit SHA in a production workflow. Keep one runner class for the series and
serialise writers. `tak push` retries by fetching and merging if another writer wins the race,
but serialisation avoids unnecessary retries. Do not cancel an in-progress main run: that
would leave a hole in the history.

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
      - uses: actions/checkout@v7
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false

      - uses: jdx/mise-action@v4

      - name: Install Valgrind
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends valgrind

      - name: Measure this pull request
        run: mise run perf:record

      - name: Find the merge base
        id: base
        env:
          BASE_REF: ${{ github.base_ref }}
        run: |
          git fetch --quiet origin "$BASE_REF"
          base=$(git merge-base "origin/$BASE_REF" HEAD)
          echo "sha=$base" >> "$GITHUB_OUTPUT"

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
          exit "$status"
```

Checking out the pull request's head SHA avoids measuring GitHub's synthetic merge commit.
Using the merge base avoids attributing unrelated changes that landed on main after the branch
was created to the pull request.

The example has a read-only token and reports through the job summary. If the workflow also
posts a sticky pull-request comment, keep the write token in a separate reporting job that
checks out no code and executes nothing from the pull request. Pass the Markdown report and
exit status to it as an artifact. mise's
[pull-request workflow](https://github.com/jdx/mise/blob/main/.github/workflows/perf-pr.yml)
shows that separation.

## Backfill published releases

A new adopter can seed history with `tak backfill` instead of rebuilding many historical
commits. This works when releases contain executable assets and one command is compatible
across the releases being measured:

```sh
tak backfill --bench release-startup --limit 20 -- --help
tak push
```

Keep backfilled release artifacts in a separate benchmark series from binaries built by the
ongoing CI workflow. Different build pipelines can produce different instruction counts even
for the same source commit.

Downloaded release binaries are untrusted executables. A workflow that backfills them should
measure in a read-only job and pass only the notes to a separate publishing job. Aube's
[backfill workflow](https://github.com/jdx/aube/blob/main/.github/workflows/perf-backfill.yml)
is the complete example.

## Check the rollout

Before treating the comparison as a required check:

- `tak doctor` reports Valgrind and the runner class you intended;
- every measured command succeeds with its network pointed at a dead port;
- setup and cache warming happen before `tak run`;
- main is the only branch pushed into `refs/notes/tak`;
- main and pull requests use the same build inputs and runner class; and
- only instruction counts gate CI; timing remains report-only.

Continue with [benchmark configuration](/guide/configuration) for every setting or
[CI and git notes](/guide/ci) for the storage plumbing.
