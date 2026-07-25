//! Pick the right release asset for a target platform.
//!
//! Given a list of release asset names and a platform, work out which one is
//! the binary you want — distinguishing `tool-x86_64-unknown-linux-musl.tar.gz`
//! from `tool-x86_64-apple-darwin.tar.gz`, `tool_amd64.deb`, and
//! `tool-linux-x86_64.tar.gz.sha256`.
//!
//! # Provenance
//!
//! The platform tables and scoring logic are extracted from
//! [mise](https://github.com/jdx/mise)'s `src/backend/asset_matcher.rs` and
//! `src/backend/platform_tokens.rs`:
//!
//! > MIT License — Copyright (c) 2025 Jeff Dickey
//!
//! mise is unmodified by this extraction; this is a copy, and it will drift.
//! It exists because asset naming is a domain where a naive heuristic is worse
//! than useless: a wrong pick yields a binary that runs and produces numbers
//! that look entirely real. mise's rules encode a great deal of accumulated
//! knowledge about how projects actually name things.
//!
//! What was left behind: checksum discovery and verification, which need an
//! HTTP client, and the `PlatformTarget`/`AssetMatcher` layer, which is bound
//! to mise's own configuration types. Everything here is pure.

mod format;
mod picker;
mod tokens;

pub use format::Format;
pub use picker::{
    AssetArch, AssetLibc, AssetOs, AssetPicker, DetectedPlatform, detect_platform_from_url,
};
pub use tokens::is_platform_or_version_token;
