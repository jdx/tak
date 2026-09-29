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

Always keep measurements partitioned by runner class. Comparing numbers across runner classes
turns an infrastructure change into an apparent code regression.

## Detect regressions that landed

`tak compare` only runs where a workflow runs it, usually on pull requests. A regression can
still reach the main branch: the check was not required, someone merged over it, a commit was
pushed directly, or several changes each stayed under the gate. `tak detect` looks at what
landed. Run it in the main-branch workflow after recording and pushing:

```sh
tak run --record
tak push
tak detect
```

It walks the first-parent history of `HEAD` (or the revision given) back to the 20th commit
with measurements (`--window` changes the count) and compares each series' consecutive
recorded points. A series is a benchmark, tool, and runner class; different runner classes
are never compared.

- **A step onto the newest commit** above the gate fails the command. No older step can: the
  run for the next commit passes, so a main workflow fails once, on the push that introduced
  the step, instead of on every push after it.
- **A step across unrecorded commits** is reported as a range, such as `a1b2c3d4e5f6..0f9e8d7c6b5a
  (3 commits)`. tak cannot tell which commit in the range caused it. A push of several commits
  records only its tip, so this is common.
- **Earlier steps** above the gate within the window are listed without failing.
- **Sustained drift** is listed without failing: the series rose above the gate across the
  window, counted from its last above-gate step, while every individual step stayed under it.

Only instruction counts are examined. Wall-clock time is shown beside each step and never
gates. The gate is the same `gate_pct` setting that `tak compare` uses.

When the newest commit has no instruction counts, or no series on it has an earlier point in
the window, the report says **Nothing was compared** and the command fails, because a check
that examined nothing would otherwise look like a pass. `--allow-empty` waives this case
only, so a step onto the newest commit still fails. `--no-gate` makes the command report
without ever failing, covering both, as it does for `tak compare`.

On the first recording, or the first on a new runner class, there is nothing earlier to
compare with. Pass `--allow-empty` for that run, or seed the history first by recording an
earlier commit on the same runner class. Other causes are configuration problems:

- The checkout has no history. The walk needs the commits between recorded points, and the
  default `actions/checkout` fetch of one commit leaves nothing to walk. The report notes when
  a shallow clone cut the walk short.
- The recording step ran without Valgrind, so it stored timing but no instruction counts.
- The commit was never recorded. Run `tak detect` after `tak run --record`.

This is a simple step detector for near-deterministic counts, not statistical change-point
detection. It does not model noise and does not look at timing metrics.
