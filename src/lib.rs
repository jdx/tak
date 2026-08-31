//! tak — benchmark command-line programs and track their performance over time.
//!
//! Pre-v1: interfaces and behavior are not finalized and may change between releases.
//!
//! Exposed as a library so the measurement and storage layers can be exercised
//! by integration tests. `measure::instructions` in particular went a long time
//! written-but-never-executed, which is exactly the failure this guards against.

pub mod artifact;
pub mod backfill;
pub mod compare;
pub mod config;
pub mod measure;
pub mod notes;
pub mod record;
pub mod settings;
