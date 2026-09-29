# CI and git notes

tak stores measurements under `refs/notes/tak` as one JSON object per line. The notes merge
with git's `cat_sort_uniq` strategy, allowing concurrent writers to deduplicate byte-identical
records without a custom service.

## Record a run

Build the subject first, then record the declared benchmarks:

```sh
tak run --record
```

Nothing leaves the machine until you push the notes:

```sh
tak push
```

## Fetch measurements

`tak history` and `tak compare` fetch remote measurements into the scratch ref
`refs/notes/tak-remote`, then merge them into the local notes without discarding records that
have not been pushed. Normally no manual fetch is needed.

For a read-only checkout that will never record measurements locally, teach its `origin`
remote to fetch the notes ref with plain `git fetch`:

```sh
tak init
git fetch
```

The notes tree uses commit SHAs as path names rather than object references. A shallow fetch of
the notes ref can therefore retrieve the full measurement history without fetching the
annotated project commits.

Do not use that direct-fetch configuration in a checkout where you run `tak run --record`:
plain git fetch has no way to merge unpushed local notes. Let `tak history`, `tak compare`, and
`tak push` use the scratch ref instead.

## Separate measurement from publication

A persistent or self-hosted runner should not receive a repository-write token while it builds
or runs project code. Export the local measurement into an artifact instead:

```sh
tak artifact export --output tak-measurement.json
```

Upload that file, then download it in a separate trusted job. The publisher must supply the
revision independently from its workflow context; the artifact is rejected if it names a
different commit:

```sh
tak artifact publish tak-measurement.json --expect "$GITHUB_SHA"
```

Publication uses the same non-forced push and `cat_sort_uniq` retry path as `tak push`, so an
old measurement and concurrent writers are preserved. The artifact format is pre-v1 and may
change between Tak releases.

## Compare revisions

Compare the current revision with the recorded baseline:

```sh
tak compare origin/main
```

An instruction-count increase beyond the configured gate fails the command. Wall-clock
changes are displayed but never gate the result. A deliberate increase can be
[accepted](#accept-an-intentional-regression) for the benchmarks it affects.

Always keep measurements partitioned by runner class. Comparing numbers across runner classes
turns an infrastructure change into an apparent code regression.

## Accept an intentional regression

Some changes make a benchmark more expensive on purpose. Rather than raising the gate for
every benchmark or reaching for `--no-gate`, name the benchmarks the change is allowed to
regress:

```sh
tak compare "$BASE_SHA" --accept startup
```

`--accept` is repeatable. Each value is one exact benchmark name. It is neither split on
commas nor trimmed, so it can accept any name, and an empty value is an error. It accepts only the benchmarks it names. Every other benchmark still gates. An acceptance names a benchmark and covers each of
its tools and runner classes. It has no size limit.

An accepted regression still appears in the report. Its row is marked `(accepted)`, and a
separate line names where each acceptance came from. It does not fail the command. The report
also lists acceptances that accepted nothing, either because the benchmark did not regress
above the gate or because no benchmark by that name was compared. A misspelt name does not fail
the command by itself, but the regression it was meant to cover still does.

### Accept from a pull-request label

The recommended CI path is a pull-request label that only maintainers can apply. The workflow
maps the label onto `--accept`. The label names the benchmark, and a maintainer decides whether
to accept the regression. For example, a GitHub Actions step can pass every
`tak-accept:NAME` label on the pull request:

```yaml
      - name: Compare and gate
        env:
          BASE_SHA: ${{ steps.base.outputs.sha }}
          LABELS: ${{ toJSON(github.event.pull_request.labels.*.name) }}
          TAK_ACCEPT_TRAILERS: "0"
        run: |
          accept=()
          while IFS= read -r label; do
            case "$label" in
              tak-accept:*) accept+=(--accept "${label#tak-accept:}") ;;
            esac
          done < <(jq -r '.[]' <<<"$LABELS")
          tak compare "$BASE_SHA" "${accept[@]}"
```

The labels are passed as JSON and read one per line, so a label for a benchmark whose name
contains a space stays whole. `TAK_ACCEPT_TRAILERS: "0"` stops a pull request from turning
trailers on in its own `tak.toml`; see below.

To re-run the gate when a label changes, add `labeled` and `unlabeled` to the workflow's
`pull_request` types. Only people with triage or write access to the repository can apply
labels.

### Accept from a commit trailer

A change can also carry its acceptance in a commit message. Put a `Tak-Accept` trailer in the
final paragraph:

```text
feat: load plugins at startup

Plugins now load eagerly so the first command does not pay for discovery.

Tak-Accept: startup
```

**Trailers are ignored unless the project opts in.** The commits that `tak compare` reads are
the change being gated. On a pull request, they are the author's own commits. If tak honoured
trailers by default, any change could waive its own gate by adding one line. Enable trailers
only when every commit in the compared range is reviewed before the result matters, or when
everyone who can push may waive the gate:

```toml
[gate]
accept_trailers = true
```

`TAK_ACCEPT_TRAILERS=1` enables trailers for one invocation. `tak.toml` is read from the
checkout being measured, so a pull request can change `accept_trailers`, just as it can change
`gate.pct`. Where the gate is enforced against changes you do not trust, set
`TAK_ACCEPT_TRAILERS=0` in the workflow. The environment takes precedence over the file.

With trailers off, the report still says when the compared range contains `Tak-Accept`
trailers. That line names them and states that they were not honoured, so an author knows why
their trailer had no effect.

With trailers on, `tak compare BASE --rev REV` reads trailers from every commit in `BASE..REV`,
including commits reached through a merge commit's second parent. List several benchmarks with
commas (`Tak-Accept: startup, resolve`) or repeat the trailer. Put the reason in the commit
body; the trailer value contains only names. The comparison needs every commit in
`BASE..REV`. A shallow checkout that omits some commits also omits their trailers, and the
regression then fails the gate.

#### Squash merges

A pull request gate reads the branch's own commits. After a squash merge, the only commit left
in history is the squashed one. Keep the trailer in the final paragraph of that commit's
message if later comparisons along the main branch should see the acceptance. git reads
trailers only from the last paragraph. A squash message that concatenates each commit's
message can leave `Tak-Accept:` in the middle of the body, where it is ordinary text. Check the
merged commit with:

```sh
git log -1 --format='%(trailers:key=Tak-Accept)'
```
