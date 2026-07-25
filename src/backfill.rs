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
use asset_picker::{AssetPicker, Format};
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
    // `proto`/`proto-redir` are the load-bearing part: `location` follows
    // redirects, and without pinning the redirect protocol a downgrade to
    // http:// on the same host would carry the Authorization header in clear.
    // Checking only the initial URL is not enough.
    let mut cfg = String::from(
        "silent\nshow-error\nlocation\nfail\nproto = \"=https\"\nproto-redir = \"=https\"\n",
    );
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

/// The libc to select assets for.
///
/// Passing `None` makes the picker assume gnu on Linux, which is right for the
/// common case and wrong on Alpine, where a gnu binary simply will not start.
/// A musl-built `tak` is a good proxy for a musl host, and it is the only
/// signal available without shelling out.
///
/// This matters beyond whether the binary runs: musl and glibc builds have
/// different allocators and different startup costs, so picking the wrong one
/// measures something the project's users never execute.
fn host_libc() -> Option<String> {
    cfg!(target_env = "musl").then(|| "musl".to_string())
}

/// Can this asset actually serve as a benchmark subject?
///
/// The picker answers "which asset fits this platform", which is not the same
/// question. It will happily choose a source tarball when that is the only
/// candidate, or a `.7z` this code cannot unpack — and an unusable pick means a
/// skipped release rather than a fallback to the next-best asset. Filtering
/// first lets the picker choose the best of what remains.
fn is_usable_subject(name: &str) -> bool {
    let n = name.to_lowercase();

    // Checksums and signatures describe a release rather than being one. The
    // picker scores them positively — verified: given only
    // `tool-x86_64-unknown-linux-gnu.tar.gz.sha256` it returns exactly that —
    // because mise consumes sidecars deliberately elsewhere. Here they are
    // never a subject.
    const SIDECAR: [&str; 9] = [
        ".sha256",
        ".sha512",
        ".sha1",
        ".md5",
        ".asc",
        ".sig",
        ".pem",
        ".sbom",
        ".intoto.jsonl",
    ];
    if SIDECAR.iter().any(|e| n.ends_with(e)) {
        return false;
    }

    // Source drops contain no executable. Whole-word so `resource-cli` survives.
    let stem = n.rsplit('/').next().unwrap_or(&n);
    if ["source", "sources", "src"].iter().any(|t| {
        stem.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w == *t)
    }) {
        return false;
    }

    // Only formats `fetch_binary` can actually open. Both sides ask `Format`
    // rather than matching suffixes independently, so a name like `.vsix` —
    // which is a zip — cannot pass the filter and then be rejected at unpack.
    match Format::from_file_name(&n) {
        // `Format::Raw` is a catch-all for "no recognised archive suffix",
        // which also covers `tool-linux-x86_64.json`, `.yaml`, `.run` and
        // anything else a project ships alongside its binaries. Those score on
        // their platform tokens, and marking one executable and running it
        // benchmarks the wrong program. A real bare binary has no extension at
        // all, or `.exe`.
        Format::Raw => {
            let base = n.rsplit('/').next().unwrap_or(&n);
            base.ends_with(".exe") || !base.contains('.')
        }
        // `tar` handles gz/xz/bz2/zst everywhere, but brotli and lz4 depend on
        // the host build. Admitting one only to fail at extraction turns a
        // usable release into a skipped one; excluding them lets the picker
        // choose a different asset instead.
        Format::Tar => !TAR_NEEDS_RARE_CODEC.iter().any(|e| n.ends_with(e)),
        Format::Zip => true,
        _ => false,
    }
}

/// Tar compressions that `tar` frequently cannot decompress unaided.
const TAR_NEEDS_RARE_CODEC: [&str; 4] = [".tar.br", ".tbr", ".tar.lz4", ".tlz4"];

pub fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    let names: Vec<String> = assets
        .iter()
        .map(|a| a.name.clone())
        .filter(|n| is_usable_subject(n))
        .collect();
    let picked = AssetPicker::with_libc(
        std::env::consts::OS.to_string(),
        std::env::consts::ARCH.to_string(),
        host_libc(),
    )
    // A macOS `.app` bundle is not something we can invoke as a subject.
    .with_no_app(true)
    .pick_best_asset(&names)?;

    assets.iter().find(|a| a.name == picked)
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

    // Same classifier the candidate filter used, so the two can never disagree.
    // Matching suffixes independently in each place is what let `.vsix` pass the
    // filter as a zip and then be rejected here as unknown.
    match Format::from_file_name(&asset.name) {
        // `tar -xf` sniffs the compression, so gz/xz/bz2/zst all work here.
        Format::Tar => run(
            Command::new("tar").args([
                "-xf",
                &archive.to_string_lossy(),
                "-C",
                &dir.to_string_lossy(),
            ]),
            "tar",
        )?,
        Format::Zip => run(
            Command::new("unzip").args([
                "-oq",
                &archive.to_string_lossy(),
                "-d",
                &dir.to_string_lossy(),
            ]),
            "unzip",
        )?,
        // The download is the artefact.
        Format::Raw => {
            make_executable(&archive)?;
            return Ok(archive);
        }
        // Unreachable via `pick_asset`, which filters these out, but a caller
        // could hand one over directly. Skipping loudly beats executing a
        // tarball as if it were a program, which is what the old suffix chain
        // did for anything it did not recognise.
        other => bail!("cannot unpack {} ({other:?})", asset.name),
    }

    let found = find_binary(dir, bin_name)
        .with_context(|| format!("no `{bin_name}` inside {}", asset.name))?;
    make_executable(&found)?;
    Ok(found)
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

        // Only meaningful on the platform this asserts against.
        if std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64" {
            let got = pick_asset(&assets).expect("something should match");
            // gnu, not musl, on a glibc host. The two builds differ in
            // allocator and startup cost, so the one users actually run is the
            // one worth measuring — and it is never the darwin asset or the
            // checksum file.
            let want = if cfg!(target_env = "musl") {
                "tool-x86_64-unknown-linux-musl.tar.gz"
            } else {
                "tool-x86_64-unknown-linux-gnu.tar.gz"
            };
            assert_eq!(got.name, want);
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
    fn source_archives_are_not_benchmark_subjects() {
        // The picker selects these when they are the only candidate; they
        // contain no executable.
        assert!(!is_usable_subject("tool-1.0-source.tar.gz"));
        assert!(!is_usable_subject("tool-src.tar.gz"));
        assert!(!is_usable_subject("sources.zip"));
        // Whole-word, so a tool whose name merely contains "src" survives.
        assert!(is_usable_subject("resource-cli-linux-x86_64.tar.gz"));
        assert!(is_usable_subject("srcery-linux-amd64.tar.gz"));
    }

    #[test]
    fn sidecars_are_never_subjects() {
        // Verified against the real picker: given only the .sha256 it returns
        // exactly that, so the filter has to catch them.
        for n in [
            "tool-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "tool-x86_64-unknown-linux-gnu.tar.gz.asc",
            "tool.sig",
            "tool.intoto.jsonl",
        ] {
            assert!(!is_usable_subject(n), "{n} should be rejected");
        }
    }

    /// The filter and `fetch_binary` must classify identically, or an asset
    /// passes here and is refused at unpack.
    #[test]
    fn vsix_is_a_zip_to_both_sides() {
        assert!(is_usable_subject("tool-linux-x86_64.vsix"));
        assert_eq!(Format::from_file_name("tool.vsix"), Format::Zip);
    }

    /// `Format::Raw` means "no recognised archive suffix", which is not the
    /// same as "an executable".
    #[test]
    fn platform_tagged_metadata_is_not_a_subject() {
        for n in [
            "tool-linux-x86_64.json",
            "tool-linux-x86_64.yaml",
            "tool-linux-x86_64.run",
            "tool-linux-x86_64.txt",
        ] {
            assert!(!is_usable_subject(n), "{n} should be rejected");
        }
        // A genuine bare binary still qualifies.
        assert!(is_usable_subject("tool-linux-x86_64"));
        assert!(is_usable_subject("tool.exe"));
    }

    #[test]
    fn tar_codecs_the_host_may_lack_are_skipped() {
        assert!(is_usable_subject("tool-linux-x86_64.tar.gz"));
        assert!(is_usable_subject("tool-linux-x86_64.tar.zst"));
        // Better to let the picker choose another asset than to fail at
        // extraction and skip the release entirely.
        assert!(!is_usable_subject("tool-linux-x86_64.tar.br"));
        assert!(!is_usable_subject("tool-linux-x86_64.tar.lz4"));
    }

    #[test]
    fn only_openable_formats_are_subjects() {
        assert!(is_usable_subject("tool-linux-x86_64.tar.gz"));
        assert!(is_usable_subject("tool-linux-x86_64.zip"));
        assert!(is_usable_subject("tool"));
        assert!(is_usable_subject("tool.exe"));
        // fetch_binary cannot open these, and picking one skips the release.
        assert!(!is_usable_subject("tool-linux-x86_64.7z"));
        assert!(!is_usable_subject("tool-linux-x86_64.gz"));
        assert!(!is_usable_subject("tool.rar"));
    }

    #[test]
    fn a_platform_asset_beats_a_source_drop() {
        let assets: Vec<Asset> = [
            "tool-1.0-source.tar.gz",
            "tool-x86_64-unknown-linux-gnu.tar.gz",
        ]
        .iter()
        .map(|n| Asset {
            name: n.to_string(),
            url: format!("https://example.invalid/{n}"),
        })
        .collect();
        if std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64" {
            assert_eq!(
                pick_asset(&assets).map(|a| a.name.as_str()),
                Some("tool-x86_64-unknown-linux-gnu.tar.gz")
            );
        }
    }

    #[test]
    fn a_release_of_only_source_yields_nothing() {
        let assets = vec![Asset {
            name: "tool-1.0-source.tar.gz".to_string(),
            url: "https://example.invalid/x".to_string(),
        }];
        assert!(pick_asset(&assets).is_none());
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
