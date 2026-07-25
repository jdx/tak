//! Archive format detection.
//!
//! A trimmed reimplementation of mise's `ExtractionFormat`. Only the two
//! questions the picker actually asks are kept — "is this a zip" and "is this
//! an archive at all" — because the picker scores formats and never extracts
//! anything. Extraction is the caller's problem.
//!
//! Written against the same extension list rather than copied, since mise's
//! version derives its parsing from `strum` and dragging that in for a
//! twenty-line enum is not a trade worth making.

/// What kind of archive, if any, a filename denotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A tarball, whatever it is compressed with.
    Tar,
    Zip,
    SevenZip,
    Rar,
    /// A single compressed file, e.g. `tool.gz` — not an archive of many.
    Compressed,
    /// A bare file: most often the executable itself.
    Raw,
}

/// Suffixes that mean "tar, possibly compressed".
const TAR_SUFFIXES: [&str; 13] = [
    ".tar", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".tbz", ".tar.zst", ".tzst",
    ".tar.br", ".tbr", ".tar.lz4",
];

/// Single-file compression, which leaves one file behind rather than a tree.
const COMPRESSED_SUFFIXES: [&str; 6] = [".gz", ".xz", ".bz2", ".zst", ".br", ".lz4"];

impl Format {
    pub fn from_file_name(filename: &str) -> Self {
        let f = filename.to_lowercase();

        // Longest match first: `.tar.gz` must not be read as `.gz`.
        if TAR_SUFFIXES.iter().any(|s| f.ends_with(s)) {
            return Format::Tar;
        }
        if f.ends_with(".zip") || f.ends_with(".vsix") {
            return Format::Zip;
        }
        if f.ends_with(".7z") {
            return Format::SevenZip;
        }
        if f.ends_with(".rar") {
            return Format::Rar;
        }
        if COMPRESSED_SUFFIXES.iter().any(|s| f.ends_with(s)) {
            return Format::Compressed;
        }
        Format::Raw
    }

    /// Does this contain multiple files, i.e. need unpacking rather than just
    /// decompressing?
    pub fn is_archive(&self) -> bool {
        matches!(
            self,
            Format::Tar | Format::Zip | Format::SevenZip | Format::Rar
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tar_variants_are_all_tar() {
        for n in [
            "t.tar",
            "t.tar.gz",
            "t.tgz",
            "t.tar.xz",
            "t.txz",
            "t.tar.bz2",
            "t.tbz",
            "t.tar.zst",
            "t.tzst",
            "t.tar.br",
            "t.tar.lz4",
        ] {
            assert_eq!(Format::from_file_name(n), Format::Tar, "{n}");
        }
    }

    /// The ordering bug this guards against: `.tar.gz` ending with `.gz` too.
    #[test]
    fn tar_gz_is_not_merely_compressed() {
        assert_eq!(Format::from_file_name("tool.tar.gz"), Format::Tar);
        assert_eq!(Format::from_file_name("tool.gz"), Format::Compressed);
    }

    #[test]
    fn zip_and_friends() {
        assert_eq!(Format::from_file_name("t.zip"), Format::Zip);
        assert_eq!(Format::from_file_name("t.vsix"), Format::Zip);
        assert_eq!(Format::from_file_name("t.7z"), Format::SevenZip);
    }

    #[test]
    fn a_bare_binary_is_raw() {
        assert_eq!(Format::from_file_name("tool"), Format::Raw);
        assert_eq!(Format::from_file_name("tool.exe"), Format::Raw);
    }

    #[test]
    fn only_multi_file_formats_count_as_archives() {
        assert!(Format::from_file_name("t.tar.gz").is_archive());
        assert!(Format::from_file_name("t.zip").is_archive());
        // One compressed file is not an archive of many.
        assert!(!Format::from_file_name("t.gz").is_archive());
        assert!(!Format::from_file_name("t").is_archive());
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(Format::from_file_name("Tool.TAR.GZ"), Format::Tar);
        assert_eq!(Format::from_file_name("Tool.ZIP"), Format::Zip);
    }
}
