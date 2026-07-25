//! tak — benchmark command-line programs and track their performance over time.
//!
//! Experimental. See README.md, which asks you to use hyperfine instead.
//!
//! Exposed as a library so the measurement and storage layers can be exercised
//! by integration tests. `measure::instructions` in particular went a long time
//! written-but-never-executed, which is exactly the failure this guards against.

pub mod measure;
pub mod notes;
pub mod record;
