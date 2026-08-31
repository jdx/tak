//! Portable handoff between a read-only measurement job and a write-capable
//! publisher.
//!
//! The publisher must not trust the artifact to choose its target commit. CI
//! supplies an independently trusted revision and publication proceeds only
//! when the artifact names that exact commit. Records are deserialised and then
//! written through [`crate::notes`], so malformed input cannot bypass Tak's
//! canonical line format or its non-forced, merge-and-retry push path.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::notes;
use crate::record::{Record, SCHEMA_VERSION};

const ARTIFACT_VERSION: u32 = 1;
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECORDS: usize = 10_000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MeasurementArtifact {
    v: u32,
    commit: String,
    records: Vec<Record>,
}

fn artifact_parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// Export every local Tak record attached to `rev` as one bounded, versioned
/// file suitable for an artifact upload.
pub fn export(path: &Path, rev: &str) -> Result<(String, usize)> {
    let commit = notes::rev_parse(rev).with_context(|| format!("cannot resolve {rev}"))?;
    let records = notes::read(None, &commit)?;
    if records.is_empty() {
        bail!("no measurements recorded for {}", &commit[..12]);
    }
    if records.len() > MAX_RECORDS {
        bail!(
            "refusing to export {} records; the artifact limit is {MAX_RECORDS}",
            records.len()
        );
    }

    let artifact = MeasurementArtifact {
        v: ARTIFACT_VERSION,
        commit: commit.clone(),
        records,
    };
    let bytes = serde_json::to_vec(&artifact)?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        bail!(
            "artifact is {} bytes; the limit is {MAX_ARTIFACT_BYTES}",
            bytes.len()
        );
    }

    let parent = artifact_parent(path);
    fs::create_dir_all(&parent)
        .with_context(|| format!("could not create {}", parent.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(&parent)
        .with_context(|| format!("could not create a temporary file in {}", parent.display()))?;
    temp.write_all(&bytes)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("could not write {}", path.display()))?;

    Ok((commit, artifact.records.len()))
}

fn read(path: &Path) -> Result<MeasurementArtifact> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("could not inspect artifact {}", path.display()))?;
    if metadata.len() > MAX_ARTIFACT_BYTES {
        bail!(
            "artifact is {} bytes; the limit is {MAX_ARTIFACT_BYTES}",
            metadata.len()
        );
    }
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let artifact: MeasurementArtifact = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a valid Tak artifact", path.display()))?;

    if artifact.v != ARTIFACT_VERSION {
        bail!(
            "unsupported artifact version {} (expected {ARTIFACT_VERSION})",
            artifact.v
        );
    }
    if artifact.records.is_empty() {
        bail!("artifact contains no measurements");
    }
    if artifact.records.len() > MAX_RECORDS {
        bail!(
            "artifact contains {} records; the limit is {MAX_RECORDS}",
            artifact.records.len()
        );
    }
    for record in &artifact.records {
        if record.v != SCHEMA_VERSION {
            bail!(
                "artifact contains record schema {} (expected {SCHEMA_VERSION})",
                record.v
            );
        }
        // Force every accepted record through the same canonical serializer the
        // notes merge strategy relies on. This also rejects non-finite metrics.
        record
            .to_line()
            .context("artifact contains an invalid record")?;
    }
    Ok(artifact)
}

/// Validate, import, and publish an artifact. `expect_rev` comes from the
/// controlling workflow, never from the measurement job or its artifact.
pub fn publish(path: &Path, expect_rev: &str, remote: &str) -> Result<(String, usize)> {
    let artifact = read(path)?;
    let expected = notes::rev_parse(expect_rev)
        .with_context(|| format!("cannot resolve expected revision {expect_rev}"))?;
    if artifact.commit != expected {
        bail!(
            "artifact targets {}, but expected {}",
            artifact.commit,
            expected
        );
    }

    notes::fetch(remote)?;
    notes::append(&expected, &artifact.records)?;
    notes::push(remote)?;
    Ok((expected, artifact.records.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn record() -> Record {
        Record {
            v: SCHEMA_VERSION,
            bench: "startup".into(),
            tool: "self".into(),
            version: None,
            runner: "test-runner".into(),
            ts: "2026-08-31T00:00:00Z".into(),
            metrics: BTreeMap::from([("instructions".into(), 123.0)]),
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = r#"{"v":1,"commit":"abc","records":[],"surprise":true}"#;
        assert!(serde_json::from_str::<MeasurementArtifact>(json).is_err());
    }

    #[test]
    fn records_roundtrip_canonically() {
        let artifact = MeasurementArtifact {
            v: ARTIFACT_VERSION,
            commit: "0".repeat(40),
            records: vec![record()],
        };
        let json = serde_json::to_string(&artifact).unwrap();
        let back: MeasurementArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.records[0].to_line().unwrap(),
            record().to_line().unwrap()
        );
    }
}
