# asset-picker

Given a list of release asset names and a target platform, work out which one is the binary
you want — distinguishing `tool-x86_64-unknown-linux-musl.tar.gz` from
`tool-x86_64-apple-darwin.tar.gz`, `tool_amd64.deb`, and `tool-linux-x86_64.tar.gz.sha256`.

> [!CAUTION]
> Extracted for [tak](https://github.com/jdx/tak), which is itself an experiment. Published so
> the version can be depended on, not because anyone should.

## Provenance

The platform tables and scoring logic are extracted from [mise](https://github.com/jdx/mise)'s
`src/backend/asset_matcher.rs` and `src/backend/platform_tokens.rs`, MIT, Copyright (c) 2025
Jeff Dickey. See `LICENSE`.

mise is unmodified by this extraction. **This is a copy and it will drift.** It exists because
asset naming is a domain where a naive heuristic is worse than useless: a wrong pick yields a
binary that runs and produces numbers that look entirely real.

Left behind: checksum discovery and verification, which need an HTTP client, and the
`PlatformTarget`/`AssetMatcher` layer, which is bound to mise's own configuration types.
Everything here is pure.

## License

MIT
