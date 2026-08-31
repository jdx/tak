# Tak GitHub Actions

Tak is pre-v1. Pin these actions to a full commit SHA and expect their inputs to change before
1.0.

## Publish a measurement artifact

Run this only in a GitHub-hosted job that checks out the trusted target revision. The action
downloads an exact Tak release archive, verifies a caller-pinned SHA-256, and invokes
`tak artifact publish` with authentication held only in process environment:

```yaml
permissions:
  contents: write
steps:
  - uses: actions/checkout@FULL_COMMIT_SHA
    with:
      fetch-depth: 0
      persist-credentials: false
  - uses: actions/download-artifact@FULL_COMMIT_SHA
    with:
      name: tak-measurement
      path: /tmp/tak-out
  - uses: jdx/tak/.github/actions/publish-artifact@FULL_COMMIT_SHA
    with:
      artifact: /tmp/tak-out/measurement.json
      expect: ${{ github.sha }}
      token: ${{ github.token }}
      version: v0.0.9
      sha256: SHA256_FROM_THE_RELEASE
```

## Report a pull request comparison

The controlling workflow must independently validate the pull request number and full head
SHA. The action treats the downloaded report as untrusted, bounds its size, verifies that its
head prefix agrees, posts one sticky comment, creates a check on the full head SHA, and fails
when the recorded comparison status is nonzero:

```yaml
permissions:
  checks: write
  pull-requests: write
steps:
  - uses: actions/download-artifact@FULL_COMMIT_SHA
    with:
      name: tak-report
      path: /tmp/tak-out
  - uses: jdx/tak/.github/actions/report-pr@FULL_COMMIT_SHA
    with:
      artifact-directory: /tmp/tak-out
      head-sha: ${{ needs.authorize.outputs.head_sha }}
      marker: <!--my-cli-perf-pr-->
      pr-number: ${{ needs.authorize.outputs.pr_number }}
      token: ${{ github.token }}
```
