# Releasing

Releases are automated with [release-plz](https://release-plz.dev). Day to day there is
nothing to do: merge to `main`, and release-plz opens a release PR that bumps versions and
writes the changelog. Merging *that* tags, publishes to crates.io, builds binaries for eight
targets, attaches them, and then publishes the GitHub release.

The release is drafted until its binaries are attached, so a release never appears without
them.

## One-time setup

Two things are needed before the automation can run, and they have to happen in this order.

### 1. Bootstrap the crates on crates.io

**crates.io can only configure a Trusted Publisher on a crate that already exists.** So the
very first publish of each crate has to use a token; every publish after that is tokenless.

The dependency order matters — `tak-cli` depends on `asset-picker`, and cargo will refuse to
package it until that name resolves on the index:

```sh
cargo login              # a temporary token from https://crates.io/settings/tokens
cargo publish -p asset-picker
cargo publish -p tak-cli
```

These are not throwaway placeholders. `0.0.1` of both crates is real, working code, so the
bootstrap publish *is* the first release rather than a stub to be replaced.

Verified ahead of time with `cargo publish --dry-run`: `asset-picker` packages to 10 files,
23 KiB compressed. `tak-cli` cannot be dry-run until `asset-picker` is on the index, which is
the same ordering constraint as above and not a fault.

### 2. Configure Trusted Publishing

On crates.io, for **each** of `asset-picker` and `tak-cli`:

> Settings → Trusted Publishing → Add a new publisher

| field | value |
|---|---|
| Repository owner | `jdx` |
| Repository name | `tak` |
| Workflow filename | `release-plz.yml` |
| Environment | *(leave blank)* |

Then **revoke the temporary token** from step 1. Once trusted publishing is configured, no
long-lived crates.io credential needs to exist anywhere — the `id-token: write` permission in
`release-plz.yml` lets `rust-lang/crates-io-auth-action` exchange a GitHub OIDC token for a
short-lived one, scoped to this repository and this workflow file.

Renaming the workflow file breaks publishing until the trusted publisher is updated to match.

### 3. Add `RELEASE_PLZ_TOKEN`

A PAT with `contents: write` and `pull-requests: write` on this repository. The built-in
`GITHUB_TOKEN` cannot be used: events it raises do not trigger other workflows, so the release
PR it opened would never run CI.

## Cutting a release by hand

You should not need to, but:

```sh
gh workflow run release.yml -f tag=v0.1.0
```

builds and attaches binaries to an existing tag, then publishes the release. Useful when a
build failed after the tag was already created — the job is idempotent and uses `--clobber`.

The undraft only happens on manual dispatch. When `release-plz.yml` calls this workflow it
publishes the release itself, once the assets are in place.

## Targets

Binaries are built for eight targets. musl is not just about portability here: `tak backfill`
runs inside slim containers, and a static binary needs no libc to match.

```
x86_64-unknown-linux-gnu     x86_64-apple-darwin
x86_64-unknown-linux-musl    aarch64-apple-darwin
aarch64-unknown-linux-gnu    x86_64-pc-windows-msvc
aarch64-unknown-linux-musl   aarch64-pc-windows-msvc
```

`SHA256SUMS` is attached alongside. `tak backfill` skips checksum sidecars when choosing an
asset, so this costs nothing downstream.

## Why the crate is `tak-cli`

The binary is `tak`. Bare `tak` on crates.io is a dormant Tak board-game implementation last
published in 2016, so the crate takes the `-cli` suffix — the same workaround as `usage-cli`
and `pitchfork-cli`.
