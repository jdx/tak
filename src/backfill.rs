//! Backfill history from published release binaries.
//!
//! A new adopter's first chart is empty, and an empty chart persuades nobody.
//! Rather than rebuilding a project at a hundred historical commits — hours of
//! compute, and impossible for anyone without a reproducible build — this
//! downloads the binaries the project already published and measures those.
//! Minutes, not hours, and it works for projects whose old commits no longer
//! build at all.
//!
//! Network and archive handling shell out to `curl` and `tar`/`unzip` for the
//! same reason [`crate::notes`] shells out to `git`: proxies, CA bundles and
//! credential helpers are already solved there, and the dependency tree stays
//! small.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    #[serde(rename = "tag_name")]
    pub tag: String,
    pub published_at: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// Releases for `repo` ("owner/name"), newest first, pre-releases excluded.
///
/// Uses the GitHub API directly rather than mise-versions: that index only
/// covers mise's curated registry, and backfill has to work for any project.
pub fn list_releases(repo: &str, limit: usize) -> Result<Vec<Release>> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
    let mut args = vec![
        "-sSL".to_string(),
        "-H".to_string(),
        "Accept: application/vnd.github+json".to_string(),
        "-H".to_string(),
        "User-Agent: tak".to_string(),
    ];
    // Unauthenticated GitHub allows 60 requests/hour, which a backfill blows
    // through immediately. Use a token when the environment offers one.
    if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        args.push("-H".to_string());
        args.push(format!("Authorization: Bearer {token}"));
    }
    args.push(url);

    let out = Command::new("curl")
        .args(&args)
        .output()
        .context("failed to run curl — is it installed?")?;
    if !out.status.success() {
        bail!("curl failed listing releases for {repo}");
    }

    let mut rels: Vec<Release> =
        serde_json::from_slice(&out.stdout).context("could not parse the GitHub releases API")?;
    rels.retain(|r| !r.prerelease && !r.assets.is_empty());
    rels.truncate(limit);
    Ok(rels)
}

/// Platform tokens for the machine we are measuring on.
///
/// Measuring a darwin binary on linux would be meaningless, so asset selection
/// is strict about the OS and only flexible about spelling.
fn platform_tokens() -> (Vec<&'static str>, Vec<&'static str>) {
    let os: Vec<&str> = match std::env::consts::OS {
        "linux" => vec!["linux"],
        "macos" => vec!["darwin", "macos", "apple", "osx"],
        "windows" => vec!["windows", "win"],
        _ => vec![],
    };
    let arch: Vec<&str> = match std::env::consts::ARCH {
        "x86_64" => vec!["x86_64", "amd64", "x64"],
        "aarch64" => vec!["aarch64", "arm64"],
        _ => vec![],
    };
    (os, arch)
}

/// Score an asset for this platform. `None` means "definitely not usable here".
///
/// Deliberately conservative: a wrong choice produces numbers that look real and
/// are meaningless, which is worse than backfilling nothing.
fn score_asset(name: &str, os: &[&str], arch: &[&str]) -> Option<i32> {
    let n = name.to_lowercase();

    // Never benchmark checksums, signatures or source archives.
    const REJECT: [&str; 8] = [
        ".sha256", ".sha512", ".asc", ".sig", ".pem", ".sbom", "sources", ".deb",
    ];
    if REJECT.iter().any(|r| n.contains(r)) {
        return None;
    }
    if n.ends_with(".rpm") || n.ends_with(".msi") || n.ends_with(".pkg") {
        return None;
    }

    if !os.is_empty() && !os.iter().any(|t| n.contains(t)) {
        return None;
    }
    if !arch.is_empty() && !arch.iter().any(|t| n.contains(t)) {
        return None;
    }

    let mut score = 100;
    // musl is statically linked, so it runs on any distro including the slim
    // container images this is most often executed in.
    if n.contains("musl") {
        score += 20;
    }
    if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
        score += 10;
    } else if n.ends_with(".tar.xz") {
        score += 8;
    } else if n.ends_with(".zip") {
        score += 5;
    }
    Some(score)
}

/// Best asset for the current platform, or `None` if nothing matches.
pub fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    let (os, arch) = platform_tokens();
    assets
        .iter()
        .filter_map(|a| score_asset(&a.name, &os, &arch).map(|s| (s, a)))
        .max_by_key(|(s, _)| *s)
        .map(|(_, a)| a)
}

fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd
        .output()
        .with_context(|| format!("failed to run {what}"))?;
    if !out.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Download `asset` into `dir`, unpack it, and return the path to `bin_name`.
///
/// Handles tarballs, zips and bare executables — the three shapes essentially
/// every CLI ships.
pub fn fetch_binary(asset: &Asset, bin_name: &str, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let archive = dir.join(&asset.name);

    run(
        Command::new("curl").args(["-sSL", "-o", &archive.to_string_lossy(), &asset.url]),
        "curl",
    )?;

    let n = asset.name.to_lowercase();
    if n.ends_with(".tar.gz") || n.ends_with(".tgz") || n.ends_with(".tar.xz") {
        run(
            Command::new("tar").args([
                "-xf",
                &archive.to_string_lossy(),
                "-C",
                &dir.to_string_lossy(),
            ]),
            "tar",
        )?;
    } else if n.ends_with(".zip") {
        run(
            Command::new("unzip").args([
                "-oq",
                &archive.to_string_lossy(),
                "-d",
                &dir.to_string_lossy(),
            ]),
            "unzip",
        )?;
    } else {
        // A bare binary: the download is the artefact.
        make_executable(&archive)?;
        return Ok(archive);
    }

    let found = find_binary(dir, bin_name)
        .with_context(|| format!("no `{bin_name}` inside {}", asset.name))?;
    make_executable(&found)?;
    Ok(found)
}

fn make_executable(p: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(p)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(p, perms)?;
    }
    #[cfg(not(unix))]
    let _ = p;
    Ok(())
}

/// Depth-limited search for `name`. Release archives nest one or two levels at
/// most; a full walk risks wandering into a vendored tree.
fn find_binary(dir: &Path, name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
        if depth > 3 {
            return None;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        let mut dirs = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                return Some(p);
            }
        }
        dirs.into_iter().find_map(|d| walk(&d, name, depth + 1))
    }
    walk(dir, name, 0)
}

/// Resolve a release tag to the commit it points at, if the tag is available
/// locally. Returns `None` rather than failing — a shallow clone legitimately
/// has no tags, and the caller can still record against something else.
pub fn tag_commit(tag: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", &format!("{tag}^{{commit}}")])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Strip a leading `v` for display. Deliberately does no other normalisation —
/// tool version strings are frequently not semver and must stay opaque.
pub fn version_of(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: [&str; 1] = ["linux"];
    const X64: [&str; 3] = ["x86_64", "amd64", "x64"];

    #[test]
    fn rejects_checksums_and_signatures() {
        for n in [
            "tool-linux-x86_64.tar.gz.sha256",
            "tool-linux-x86_64.tar.gz.asc",
            "tool.sbom.json",
        ] {
            assert_eq!(score_asset(n, &LINUX, &X64), None, "{n} should be rejected");
        }
    }

    #[test]
    fn rejects_other_platforms() {
        assert_eq!(score_asset("tool-darwin-arm64.tar.gz", &LINUX, &X64), None);
        assert_eq!(score_asset("tool-linux-aarch64.tar.gz", &LINUX, &X64), None);
    }

    #[test]
    fn rejects_distro_packages() {
        // These need installing, not extracting, and would silently fail later.
        assert_eq!(score_asset("tool_1.2.3_amd64.deb", &LINUX, &X64), None);
        assert_eq!(score_asset("tool-1.2.3.x86_64.rpm", &LINUX, &X64), None);
    }

    #[test]
    fn prefers_musl_over_gnu() {
        let musl = score_asset("tool-x86_64-unknown-linux-musl.tar.gz", &LINUX, &X64).unwrap();
        let gnu = score_asset("tool-x86_64-unknown-linux-gnu.tar.gz", &LINUX, &X64).unwrap();
        assert!(musl > gnu, "musl {musl} should outrank gnu {gnu}");
    }

    #[test]
    fn prefers_tarball_over_zip() {
        let tgz = score_asset("tool-linux-amd64.tar.gz", &LINUX, &X64).unwrap();
        let zip = score_asset("tool-linux-amd64.zip", &LINUX, &X64).unwrap();
        assert!(tgz > zip);
    }

    #[test]
    fn accepts_common_arch_spellings() {
        for n in [
            "tool-linux-x86_64.tar.gz",
            "tool-linux-amd64.tar.gz",
            "tool-linux-x64.tar.gz",
        ] {
            assert!(score_asset(n, &LINUX, &X64).is_some(), "{n} should match");
        }
    }

    #[test]
    fn pick_asset_chooses_the_best_candidate() {
        let assets: Vec<Asset> = [
            "tool-x86_64-apple-darwin.tar.gz",
            "tool-x86_64-unknown-linux-gnu.tar.gz",
            "tool-x86_64-unknown-linux-musl.tar.gz",
            "tool-x86_64-unknown-linux-musl.tar.gz.sha256",
        ]
        .iter()
        .map(|n| Asset {
            name: n.to_string(),
            url: format!("https://example.invalid/{n}"),
        })
        .collect();

        // Only meaningful on the platform whose tokens the test asserts.
        if std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64" {
            let got = pick_asset(&assets).expect("something should match");
            assert_eq!(got.name, "tool-x86_64-unknown-linux-musl.tar.gz");
        }
    }

    #[test]
    fn version_strips_only_the_v_prefix() {
        assert_eq!(version_of("v1.2.3"), "1.2.3");
        assert_eq!(version_of("2024.01.15"), "2024.01.15");
        // Non-semver tags must survive untouched.
        assert_eq!(version_of("nightly"), "nightly");
        assert_eq!(version_of("lts-iron"), "lts-iron");
    }

    #[test]
    fn finds_a_nested_binary() {
        let dir = std::env::temp_dir().join(format!("tak-find-{}", std::process::id()));
        let nested = dir.join("tool-1.0").join("bin");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("mycli"), b"#!/bin/sh\n").unwrap();

        assert_eq!(find_binary(&dir, "mycli"), Some(nested.join("mycli")));
        assert_eq!(find_binary(&dir, "absent"), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
