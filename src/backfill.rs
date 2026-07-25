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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// Drafts are visible to authenticated requests and are not published
    /// artefacts, so benchmarking one would record a version nobody can install.
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// GitHub's maximum, and the fewest round trips per page of history.
const PER_PAGE: usize = 100;
/// Bound the walk so a `--limit` larger than a project's history cannot spin.
const MAX_PAGES: usize = 10;

fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
        .filter(|t| !t.is_empty())
}

/// Run curl, keeping the bearer token off the command line.
///
/// The split matters. A token passed as `-H` lands in argv, where any local
/// process reading `/proc` recovers it, so it goes over stdin via `--config -`.
/// The URL and output path are *not* secret and go as ordinary arguments —
/// which also sidesteps curl's config quoting, where a value containing `"` or
/// a newline would otherwise close the quoted field and inject further
/// directives. A crafted `browser_download_url` could then add its own `url =`
/// line and receive the Authorization header meant for GitHub.
fn curl(url: &str, output: Option<&Path>, auth: bool) -> Result<Vec<u8>> {
    let mut cfg = String::from("silent\nshow-error\nlocation\nfail\n");
    cfg.push_str("header = \"User-Agent: tak\"\n");
    if auth && let Some(token) = github_token() {
        // curl's config format has no escape for `"` inside a quoted value, and
        // GitHub tokens are ASCII-alphanumeric with `_`, so a token containing
        // a quote is malformed rather than something to escape.
        if token.contains(['"', '\n', '\r']) {
            bail!("refusing to use a token containing quotes or newlines");
        }
        cfg.push_str(&format!("header = \"Authorization: Bearer {token}\"\n"));
    }

    // Only ever talk to https endpoints: a `http://` or `file://` redirect
    // target would send the header in clear or read local files.
    if !url.starts_with("https://") {
        bail!("refusing a non-https URL: {url}");
    }

    let mut cmd = Command::new("curl");
    cmd.arg("--config").arg("-");
    if let Some(p) = output {
        cmd.arg("-o").arg(p);
    }
    cmd.arg("--").arg(url);

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run curl — is it installed?")?;
    child
        .stdin
        .take()
        .context("curl stdin unavailable")?
        .write_all(cfg.as_bytes())?;

    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Releases for `repo` ("owner/name"), newest first, pre-releases excluded.
///
/// Pages until `limit` qualifying releases are found or the history runs out —
/// a first page consisting mostly of pre-releases would otherwise silently
/// return far fewer than asked for.
///
/// Uses the GitHub API directly rather than mise-versions: that index only
/// covers mise's curated registry, and backfill has to work for any project.
pub fn list_releases(repo: &str, limit: usize) -> Result<Vec<Release>> {
    let mut out: Vec<Release> = Vec::new();

    for page in 1..=MAX_PAGES {
        let url =
            format!("https://api.github.com/repos/{repo}/releases?per_page={PER_PAGE}&page={page}");
        let body = curl(&url, None, true)?;
        let batch: Vec<Release> =
            serde_json::from_slice(&body).context("could not parse the GitHub releases API")?;
        let exhausted = batch.len() < PER_PAGE;

        out.extend(
            batch
                .into_iter()
                .filter(|r| !r.prerelease && !r.draft && !r.assets.is_empty()),
        );

        if out.len() >= limit {
            break;
        }
        if exhausted {
            return Ok(finish(out, limit));
        }
        if page == MAX_PAGES {
            eprintln!(
                "warning: stopped after {MAX_PAGES} pages with {} of {limit} releases; \
                 older releases exist but were not fetched",
                out.len()
            );
        }
    }

    Ok(finish(out, limit))
}

fn finish(mut v: Vec<Release>, limit: usize) -> Vec<Release> {
    v.truncate(limit);
    v
}

/// Does `haystack` contain `token` as a whole word?
///
/// Plain `contains` is wrong here: the Windows token `win` is a substring of
/// `apple-darwin`, so a macOS tarball would satisfy the Windows filter and get
/// measured on the wrong platform. Requiring non-alphanumeric boundaries makes
/// `win` match `tool-win.zip` but not `darwin`, while `win64` still matches
/// `tool-win64.zip` as its own token.
fn contains_token(haystack: &str, token: &str) -> bool {
    let h = haystack.as_bytes();
    haystack.match_indices(token).any(|(i, _)| {
        let before = i == 0 || !h[i - 1].is_ascii_alphanumeric();
        let end = i + token.len();
        let after = end == h.len() || !h[end].is_ascii_alphanumeric();
        before && after
    })
}

/// Platform tokens for the machine we are measuring on.
///
/// Measuring a darwin binary on linux would be meaningless, so asset selection
/// is strict about the OS and only flexible about spelling.
fn platform_tokens() -> (Vec<&'static str>, Vec<&'static str>) {
    let os: Vec<&str> = match std::env::consts::OS {
        "linux" => vec!["linux"],
        "macos" => vec!["darwin", "macos", "apple", "osx"],
        "windows" => vec!["windows", "win64", "win32", "win"],
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
    const REJECT: [&str; 7] = [
        ".sha256", ".sha512", ".asc", ".sig", ".pem", ".sbom", ".deb",
    ];
    if REJECT.iter().any(|r| n.contains(r)) {
        return None;
    }
    // Source drops contain no executable. Matched as whole words so a tool
    // legitimately named e.g. `resource-cli` is not rejected.
    if ["source", "sources", "src"]
        .iter()
        .any(|t| contains_token(&n, t))
    {
        return None;
    }
    if n.ends_with(".rpm") || n.ends_with(".msi") || n.ends_with(".pkg") {
        return None;
    }

    if !os.is_empty() && !os.iter().any(|t| contains_token(&n, t)) {
        return None;
    }
    if !arch.is_empty() && !arch.iter().any(|t| contains_token(&n, t)) {
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
/// every CLI ships. The download is authenticated for the same reason the
/// listing is: a private repository's assets are not publicly readable, and
/// failing here after listing succeeded would skip every release.
pub fn fetch_binary(asset: &Asset, bin_name: &str, dir: &Path) -> Result<PathBuf> {
    // Start clean. Reusing a populated directory risks `find_binary` picking up
    // an executable left by an earlier release and attributing its measurement
    // to the wrong tag.
    std::fs::remove_dir_all(dir).ok();
    std::fs::create_dir_all(dir)?;
    // Asset names come from the API and are not trusted: `../../x` would escape
    // the per-release directory and write wherever it liked.
    let archive = dir.join(safe_component(&asset.name)?);

    curl(&asset.url, Some(&archive), true)?;

    let n = asset.name.to_lowercase();
    if TAR_SUFFIXES.iter().any(|e| n.ends_with(e)) {
        // `tar -xf` sniffs the compression, so gz/xz/bz2/zst all work here.
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
    } else if is_bare_binary(&n) {
        // The download is the artefact.
        make_executable(&archive)?;
        return Ok(archive);
    } else {
        // Anything else would previously have been *executed* as if it were a
        // binary. Skipping loudly beats benchmarking a tarball.
        bail!("unsupported archive format: {}", asset.name);
    }

    let found = find_binary(dir, bin_name)
        .with_context(|| format!("no `{bin_name}` inside {}", asset.name))?;
    make_executable(&found)?;
    Ok(found)
}

/// Archive suffixes `tar` can unpack unaided.
const TAR_SUFFIXES: [&str; 7] = [
    ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tar.zst", ".tar",
];

/// Is this plausibly a bare executable rather than an archive?
///
/// Conservative on purpose: an unrecognised extension is treated as an archive
/// we cannot open, not as something safe to run.
fn is_bare_binary(lower_name: &str) -> bool {
    if lower_name.ends_with(".exe") {
        return true;
    }
    // No extension at all — the usual shape for a raw unix binary.
    !Path::new(lower_name)
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|f| f.contains('.'))
}

/// Reduce an untrusted name to a single safe path component.
///
/// `Path::join` with a value containing `..` or separators escapes the intended
/// directory, and both asset names and tags come from the GitHub API.
fn safe_component(name: &str) -> Result<String> {
    let base = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .with_context(|| format!("unusable name from the API: {name:?}"))?;
    Ok(base.to_string())
}

/// A filesystem-safe, collision-free directory name for a release.
///
/// Sanitising alone is not enough: tags may contain `/` (`release/1.0`), and
/// mapping both `v1/0` and `v1_0` to `v1_0` would let two releases share a
/// directory. The index keeps them distinct.
pub fn release_dir_name(index: usize, tag: &str) -> String {
    format!("{index:04}-{}", safe_dir_name(tag))
}

/// A filesystem-safe directory name for a release tag.
///
/// Tags may contain `/` (`release/1.0`) and are otherwise arbitrary, so anything
/// outside a conservative set becomes `_`.
fn safe_dir_name(tag: &str) -> String {
    let s: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = s.trim_matches('.');
    if trimmed.is_empty() {
        "release".to_string()
    } else {
        trimmed.to_string()
    }
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

/// Names to accept for the executable.
///
/// Windows archives ship `tool.exe`, while `--bin` defaults to the repository
/// name without a suffix, so an exact-match-only search skips every Windows
/// release.
fn binary_candidates(name: &str) -> Vec<String> {
    let mut v = vec![name.to_string()];
    if cfg!(windows) && !name.ends_with(".exe") {
        v.push(format!("{name}.exe"));
    }
    v
}

/// Depth-limited search for the executable. Release archives nest one or two
/// levels at most; a full walk risks wandering into a vendored tree.
fn find_binary(dir: &Path, name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, names: &[String], depth: u32) -> Option<PathBuf> {
        if depth > 3 {
            return None;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        let mut dirs = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|f| names.iter().any(|n| n == f))
            {
                return Some(p);
            }
        }
        dirs.into_iter().find_map(|d| walk(&d, names, depth + 1))
    }
    walk(dir, &binary_candidates(name), 0)
}

/// Whether the current directory is inside a git work tree.
///
/// Distinguishes "not in a repository" from "tag not fetched", which otherwise
/// both surface as a missing tag and send people looking in the wrong place.
pub fn in_git_repo() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve a release tag to the commit it points at, if the tag is available
/// locally. Returns `None` rather than failing — a shallow clone legitimately
/// has no tags, and the caller can still report that precisely.
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
    const WINDOWS: [&str; 4] = ["windows", "win64", "win32", "win"];

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

    /// `win` is a substring of `darwin`, so substring matching would let a macOS
    /// tarball satisfy the Windows filter and be measured on the wrong platform.
    #[test]
    fn win_token_does_not_match_darwin() {
        assert_eq!(
            score_asset("tool-x86_64-apple-darwin.tar.gz", &WINDOWS, &X64),
            None,
            "a darwin asset must never satisfy the windows filter"
        );
        assert!(score_asset("tool-x86_64-win64.zip", &WINDOWS, &X64).is_some());
        assert!(score_asset("tool-windows-amd64.zip", &WINDOWS, &X64).is_some());
        assert!(score_asset("tool-win-x64.zip", &WINDOWS, &X64).is_some());
    }

    #[test]
    fn token_matching_respects_word_boundaries() {
        assert!(contains_token("tool-linux-amd64.tar.gz", "linux"));
        assert!(contains_token("linux", "linux"));
        assert!(!contains_token("apple-darwin", "win"));
        assert!(!contains_token("mylinuxish", "linux"));
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
    fn windows_archives_may_carry_an_exe_suffix() {
        let c = binary_candidates("mycli");
        assert!(c.contains(&"mycli".to_string()));
        if cfg!(windows) {
            assert!(c.contains(&"mycli.exe".to_string()));
        }
        // An explicit .exe must not become mycli.exe.exe.
        assert_eq!(binary_candidates("mycli.exe").len(), 1);
    }

    #[test]
    fn release_dirs_never_collide() {
        // Sanitising alone maps both of these to `v1_0`.
        assert_ne!(
            release_dir_name(0, "v1/0"),
            release_dir_name(1, "v1_0"),
            "distinct releases must not share a work directory"
        );
        assert_eq!(release_dir_name(7, "v1.2.3"), "0007-v1.2.3");
        // Path separators must not survive into the name.
        assert!(!release_dir_name(0, "release/1.0").contains('/'));
        // A tag of only punctuation still yields something usable.
        assert!(!release_dir_name(0, "...").is_empty());
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
