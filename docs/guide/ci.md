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
every benchmark or reaching for `--no-gate`, name the benchmark the change is allowed to
regress. Put a `Tak-Accept` trailer in the final paragraph of a commit message:

```text
feat: load plugins at startup

Plugins now load eagerly so the first command does not pay for discovery.

Tak-Accept: startup
```

`tak compare BASE --rev REV` reads the trailer from every commit in `BASE..REV`, including
commits reached through a merge commit's second parent. The trailer accepts only the
benchmarks it names. List several benchmarks with commas (`Tak-Accept: startup, resolve`) or
repeat the trailer. Put the reason in the commit body; the trailer value contains only names.

A CI integration can pass the same acceptance from outside the commits, such as a
pull-request label, with the repeatable `--accept` flag:

```sh
tak compare "$BASE_SHA" --accept startup
```

An accepted regression is still shown in the report. Its row is marked `(accepted)`, and a
separate line names the source of each acceptance: `--accept`, or the commit carrying the
trailer. It does not fail the command. Every other benchmark still gates. An acceptance names a
benchmark and covers each of its tools and runner classes; it has no size limit. The report
also lists acceptances that accepted nothing, either because the benchmark did not regress
above the gate or because no benchmark by that name was compared. A misspelt name does not fail
the command by itself, but the regression it was meant to cover still does.

Anyone who can write a commit message in the range can add the trailer, including the author
of the pull request being gated. tak does not restrict who may accept a regression, and it has
no option to ignore trailers. The acceptance is visible in the report and in history, so it is
reviewed only when the commit that carries it is reviewed.

The comparison needs the commits in `BASE..REV`. A shallow checkout that omits some of them
also omits their trailers, and the regression fails the gate.

### Squash merges

A pull request gate reads the branch's own commits. After a squash merge, the only commit left
in history is the squashed one. Keep the trailer in the final paragraph of that commit's message
if later comparisons along the main branch should see the acceptance. git reads trailers only
from the last paragraph. A squash message that concatenates each commit's message can leave
`Tak-Accept:` in the middle of the body, where it is ordinary text. Check the merged commit
with:

```sh
git log -1 --format='%(trailers:key=Tak-Accept)'
```
