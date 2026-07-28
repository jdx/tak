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

Teach the repository's `origin` remote to fetch the notes ref:

```sh
tak init
git fetch
```

Or fetch it explicitly:

```sh
git fetch --depth 1 origin '+refs/notes/tak:refs/notes/tak'
```

The notes tree uses commit SHAs as path names rather than object references. A shallow fetch of
the notes ref can therefore retrieve the full measurement history without fetching the
annotated project commits.

## Compare revisions

Compare the current revision with the recorded baseline:

```sh
tak compare origin/main
```

An instruction-count increase beyond the configured gate fails the command. Wall-clock
changes are displayed but never gate the result.

Always keep measurements partitioned by runner class. Comparing numbers across runner classes
turns an infrastructure change into an apparent code regression.
