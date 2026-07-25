# Releasing

Releases are automated with [release-plz](https://release-plz.dev). Merge to `main`, and
release-plz opens a release PR that bumps versions and writes the changelog. Merging *that*
tags, publishes to crates.io, builds binaries for eight targets, attaches them, and publishes
the GitHub release.

The release stays a draft until its binaries are attached, so it never appears without them.

## Crates

| crate | what it is |
|---|---|
| `tak-cli` | The binary. Named `tak-cli` because bare `tak` on crates.io is a dormant 2016 board-game implementation — the same workaround as `usage-cli` and `pitchfork-cli`. The installed binary is `tak`. |
| `asset-picker` | The release-asset selection library. Published to crates.io, but gets no GitHub release: it has no assets to attach. |

## Publishing credentials

There are none. Publishing uses crates.io trusted publishing: `id-token: write` in
`release-plz.yml` lets `rust-lang/crates-io-auth-action` exchange a GitHub OIDC token for a
short-lived crates.io one, scoped to this repository and this workflow file.

Each crate has a Trusted Publisher on crates.io pointing at `jdx/tak` and the workflow
filename `release-plz.yml`. **Renaming that workflow breaks publishing** until the trusted
publisher is updated to match.

The only stored secret is `RELEASE_PLZ_TOKEN`, a PAT with `contents: write` and
`pull-requests: write`. The built-in `GITHUB_TOKEN` cannot be used: events it raises do not
trigger other workflows, so the release PR it opened would never run CI.

## Building a release by hand

```sh
gh workflow run release.yml -f tag=v0.1.0
```

Builds all eight targets, attaches them with `--clobber`, and publishes the release. It is
idempotent. The tag and the GitHub release must already exist — release-plz creates both, and
this workflow only rebuilds and reattaches.

The undraft happens only on manual dispatch — when `release-plz.yml` calls the same workflow,
it publishes the release itself once the assets are in place.

`release-plz.yml` can also be dispatched manually, but that cannot conjure a release out of
nothing: release-plz compares packaged file contents against crates.io and opens a release PR
only when they differ. "already up to date" means the published crate really is identical.

## Targets

Eight targets. musl is not only about portability: `tak backfill` runs inside slim containers,
and a static binary needs no libc to match.

```
x86_64-unknown-linux-gnu     x86_64-apple-darwin
x86_64-unknown-linux-musl    aarch64-apple-darwin
aarch64-unknown-linux-gnu    x86_64-pc-windows-msvc
aarch64-unknown-linux-musl   aarch64-pc-windows-msvc
```

`SHA256SUMS` is attached alongside. `tak backfill` skips checksum sidecars when choosing an
asset, so this costs nothing downstream.
