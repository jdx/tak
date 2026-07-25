# Releasing

Releases are automated with [release-plz](https://release-plz.dev). Merge to `main`, and
release-plz opens a release PR that bumps versions and writes the changelog. Merging *that*
tags, publishes to crates.io, builds binaries for seven targets, attaches them, rewrites the
release notes, and publishes the GitHub release.

The release stays a draft until its binaries are attached and its notes are written, so it
never appears half-finished.

## Cadence

Nobody merges the release PR by hand. `auto-merge-release.yml` runs daily at 10:00 UTC and
merges it only if **both** hold:

- the most recent `v*` tag is at least **seven days** old, and
- at least one `fix:` or `feat:` commit has landed since it

Docs, CI and chore commits therefore accumulate without producing a release. To release
sooner, dispatch it manually — that skips both guards:

```sh
gh workflow run auto-merge-release.yml
```

## Release notes

`release-plz` writes the release body from commit subjects using `cliff.toml`.
[communique](https://github.com/jdx/communique) then rewrites it into prose from the commits,
pull requests and diffs, in the `enhance-release` job. It runs while the release is still a
draft and in parallel with the binary builds, so it costs no wall clock and the raw version is
never visible.

Tone and project context live in `communique.toml`. The tool version is pinned in `mise.toml` —
that is the only thing mise is used for in this repository.

The job is `continue-on-error`, so if generation fails the release still publishes, with the
`cliff.toml` notes intact.

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

Two secrets are stored:

| secret | used by | why |
|---|---|---|
| `RELEASE_PLZ_TOKEN` | `release-plz.yml`, `auto-merge-release.yml` | A PAT with `contents: write` and `pull-requests: write`. The built-in `GITHUB_TOKEN` cannot be used: events it raises do not trigger other workflows, so the release PR it opened would never run CI. communique also needs it to read the *draft* release — the `/releases/tags` endpoint hides drafts, so it falls back to listing releases, which requires write access. |
| `ANTHROPIC_API_KEY` | `release-plz.yml` (`enhance-release`) | communique's model calls. Without it, note generation fails and the release publishes with the `cliff.toml` body. |

## Building a release by hand

```sh
gh workflow run release.yml -f tag=v0.1.0
```

Builds all seven targets, attaches them with `--clobber`, and publishes the release. It is
idempotent. The tag and the GitHub release must already exist — release-plz creates both, and
this workflow only rebuilds and reattaches.

The undraft happens only on manual dispatch — when `release-plz.yml` calls the same workflow,
it publishes the release itself once the assets are in place.

`release-plz.yml` can also be dispatched manually, but that cannot conjure a release out of
nothing: release-plz compares packaged file contents against crates.io and opens a release PR
only when they differ. "already up to date" means the published crate really is identical.

## Targets

Seven targets. musl is not only about portability: `tak backfill` runs inside slim containers,
and a static binary needs no libc to match.

```
x86_64-unknown-linux-gnu     aarch64-apple-darwin
x86_64-unknown-linux-musl    x86_64-pc-windows-msvc
aarch64-unknown-linux-gnu    aarch64-pc-windows-msvc
aarch64-unknown-linux-musl
```

macOS is Apple Silicon only. Intel Macs are not a target we publish for.

`SHA256SUMS` is attached alongside. `tak backfill` skips checksum sidecars when choosing an
asset, so this costs nothing downstream.
