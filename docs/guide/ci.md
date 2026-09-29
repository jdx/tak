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

`tak history`, `tak log`, and `tak compare` fetch remote measurements into the scratch ref
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
plain git fetch has no way to merge unpushed local notes. Let `tak history`, `tak log`,
`tak compare`, and `tak push` use the scratch ref instead.

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

## Browse history

`tak history` shows the records on one commit. `tak log` shows how each benchmark changed
over time: it walks the first-parent history of a revision (`HEAD` by default) and prints one
table per benchmark and runner class, newest commit first. Commits with nothing recorded are
skipped. Several records for one series on one commit reduce to the minimum, the same rule
`tak compare` uses.

```sh
tak log HEAD~10 -n 5 --bench startup
```

On a scratch repository with synthetic measurements recorded on most commits, with the footer
lines omitted:

```
5 recorded commit(s) on the first-parent history of `HEAD~10`, 2026-08-09 to 2026-08-14 (14 commit(s) walked). 6 older recorded commit(s) are not shown; `-n` shows more.

### `startup` on `gha-linux-x64`

| commit    | date       | instructions |      Δ | wall (min) | subject |
|-----------|------------|-------------:|-------:|-----------:|---------|
| `cd025ff` | 2026-08-14 |   30,014,101 | +0.00% |    11.86ms | fix(run): keep stderr on failure (#114) |
| `8801dc0` | 2026-08-12 |   30,013,263 | +2.03% |    11.90ms | perf: cache parsed manifests (#112) |
| `09c874e` | 2026-08-11 |   29,415,344 | -0.01% |    13.27ms | refactor: split config loader (#111) |
| `e2ce659` | 2026-08-10 |   29,417,425 | +0.01% |    11.49ms | chore(deps): update serde (#110) |
| `1b45311` | 2026-08-09 |   29,414,506 |      — |    11.07ms | fix: skip empty lockfile (#109) |
```

Δ is the change in instruction count from the series' previous measurement, which may be
several commits earlier. `-n` counts recorded commits, not commits walked. The output is
Markdown, so it can be appended to `$GITHUB_STEP_SUMMARY`.

A series never crosses runner classes: moving to a new runner class starts a new table rather
than a jump in an existing one. A clone made with `--depth` only has part of the history. When
the walk runs out of commits in a shallow clone, `tak log` says that older measurements may
exist. Check out with `fetch-depth: 0` in CI.

`tak log` only displays measurements. It does not detect regressions, and it does not gate.

## Publish a history report

`tak log --html PATH` writes the same history as one self-contained HTML page. Each series
gets an instruction-count chart and a smaller wall-time chart beneath it. The page has no
scripts and no external stylesheets or fonts. It follows the viewer's light or dark preference,
and hovering a point shows its commit, subject, and values. When `origin` is a GitHub
repository, points and table rows link to the commit.

The report's format is pre-v1 and may change between tak releases.

To publish it to GitHub Pages after each main-branch recording, add two jobs to the
main-branch workflow from [Adopt tak in a project](/guide/adopting#record-the-main-branch),
and set the repository's Pages source to GitHub Actions:

```yaml
  report:
    needs: measure
    runs-on: ubuntu-24.04
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          # The first-parent walk needs the commits, not only the notes.
          fetch-depth: 0
          persist-credentials: false

      - uses: jdx/mise-action@dad1bfd3df957f44999b559dd69dc1671cb4e9ea # v4.2.1

      - name: Render the report
        run: |
          mkdir -p site
          tak log -n 200 --html site/index.html

      - uses: actions/upload-pages-artifact@fc324d3547104276b827a68afc52ff2a11cc49c9 # v5
        with:
          path: site

  deploy-report:
    needs: report
    runs-on: ubuntu-24.04
    permissions:
      pages: write
      id-token: write
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - id: deployment
        uses: actions/deploy-pages@368f82528645a54fb793d4d04e342629a3f51346 # v5.0.1
```

`needs: measure` orders rendering after the measurement job pushes its notes. `tak log`
fetches `refs/notes/tak` from `origin` itself. That fetch is unauthenticated here because the
checkout does not keep its credentials, which is enough for a public repository. For a
private repository, fetch the notes before removing credentials, as the pull-request example
in [Adopt tak in a project](/guide/adopting#gate-pull-requests) does.

A repository has one Pages site. If Pages already serves the project's documentation, write the
report into that site's build, for example as `perf/index.html`, instead of deploying it
separately.
