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
changes are displayed but never gate the result.

Each benchmark can have its own gate in `tak.toml`: a different percentage, an absolute
`min_delta` floor, or `enabled = false` to report a benchmark without failing on it. See
[per-benchmark gates](/guide/configuration#per-benchmark-gates). `tak compare` reads these from
the `tak.toml` in the working tree, not from the notes or from the base revision. In a
pull-request job that checks out the head, the pull request's own `tak.toml` decides, so a
change to a gate is part of the diff under review.

When some benchmark's gate differs from `[gate]`, the verdict says
`N benchmark(s) above their gate` instead of `N benchmark(s) above the 1% gate`, and a
report-only benchmark that rose is listed as `N report-only benchmark(s) above their gate`.
A script that greps the report for a regression should match both forms. The exit status is
simpler to rely on: `tak compare` exits non-zero only for a regression in a gated benchmark, or
for an error.

To see which functions a change came from, keep cachegrind's profiles on both sides; see
[Explain an instruction-count change](./attribution).

Always keep measurements partitioned by runner class. Comparing numbers across runner classes
turns an infrastructure change into an apparent code regression.
