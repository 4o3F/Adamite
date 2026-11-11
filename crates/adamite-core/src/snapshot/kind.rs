//! File categories derived from the shared filename-extension rules.

use super::{QueryError, rules::Extension};
use std::str::FromStr;

/// Category inferred from a filename extension, with unknown suffixes grouped as Other.
///
/// Declaration order breaks allocation ties in kind statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum FileKind {
    /// Video files.
    Video,
    /// Audio files.
    Audio,
    /// Raster, vector and layered images.
    Image,
    /// Compressed files and archives.
    Archive,
    /// Disk, virtual disk and installation images.
    DiskImage,
    /// Documents, spreadsheets, presentations and plain text.
    Document,
    /// Source code, scripts and structured configuration.
    Code,
    /// Programs, libraries, drivers and command files.
    Executable,
    /// Files with an unknown or absent extension.
    #[default]
    Other,
}

impl FileKind {
    /// All categories in the order used to break allocation ties.
    pub const ALL: [Self; 9] = [
        Self::Video,
        Self::Audio,
        Self::Image,
        Self::Archive,
        Self::DiskImage,
        Self::Document,
        Self::Code,
        Self::Executable,
        Self::Other,
    ];

    /// Returns the lowercase category name accepted by parsing.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Image => "image",
            Self::Archive => "archive",
            Self::DiskImage => "disk-image",
            Self::Document => "document",
            Self::Code => "code",
            Self::Executable => "executable",
            Self::Other => "other",
        }
    }

    pub(super) fn from_extension(extension: &Extension<'_>) -> Self {
        EXTENSION_KINDS
            .binary_search_by(|(key, _)| extension.compare_folded(key))
            .ok()
            .and_then(|position| EXTENSION_KINDS.get(position))
            .map_or(Self::Other, |(_, kind)| *kind)
    }
}

impl FromStr for FileKind {
    type Err = QueryError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.name() == name)
            .ok_or(QueryError::InvalidKind)
    }
}

pub(super) static EXTENSION_KINDS: &[(&str, FileKind)] = &[
    ("7z", FileKind::Archive),
    ("aac", FileKind::Audio),
    ("avi", FileKind::Video),
    ("bat", FileKind::Executable),
    ("bmp", FileKind::Image),
    ("bz2", FileKind::Archive),
    ("c", FileKind::Code),
    ("cab", FileKind::Archive),
    ("cmd", FileKind::Executable),
    ("cpp", FileKind::Code),
    ("cs", FileKind::Code),
    ("css", FileKind::Code),
    ("csv", FileKind::Document),
    ("dll", FileKind::Executable),
    ("doc", FileKind::Document),
    ("docx", FileKind::Document),
    ("dylib", FileKind::Executable),
    ("epub", FileKind::Document),
    ("esd", FileKind::DiskImage),
    ("exe", FileKind::Executable),
    ("flac", FileKind::Audio),
    ("flv", FileKind::Video),
    ("gif", FileKind::Image),
    ("go", FileKind::Code),
    ("gz", FileKind::Archive),
    ("h", FileKind::Code),
    ("heic", FileKind::Image),
    ("hpp", FileKind::Code),
    ("html", FileKind::Code),
    ("img", FileKind::DiskImage),
    ("iso", FileKind::DiskImage),
    ("java", FileKind::Code),
    ("jpeg", FileKind::Image),
    ("jpg", FileKind::Image),
    ("js", FileKind::Code),
    ("json", FileKind::Code),
    ("m4a", FileKind::Audio),
    ("m4v", FileKind::Video),
    ("md", FileKind::Document),
    ("mkv", FileKind::Video),
    ("mov", FileKind::Video),
    ("mp3", FileKind::Audio),
    ("mp4", FileKind::Video),
    ("msi", FileKind::Executable),
    ("odt", FileKind::Document),
    ("ogg", FileKind::Audio),
    ("opus", FileKind::Audio),
    ("pdf", FileKind::Document),
    ("png", FileKind::Image),
    ("ppt", FileKind::Document),
    ("pptx", FileKind::Document),
    ("ps1", FileKind::Code),
    ("psd", FileKind::Image),
    ("py", FileKind::Code),
    ("qcow2", FileKind::DiskImage),
    ("rar", FileKind::Archive),
    ("raw", FileKind::Image),
    ("rs", FileKind::Code),
    ("rtf", FileKind::Document),
    ("sh", FileKind::Code),
    ("so", FileKind::Executable),
    ("svg", FileKind::Image),
    ("sys", FileKind::Executable),
    ("tar", FileKind::Archive),
    ("tif", FileKind::Image),
    ("tiff", FileKind::Image),
    ("toml", FileKind::Code),
    ("ts", FileKind::Video),
    ("txt", FileKind::Document),
    ("vhd", FileKind::DiskImage),
    ("vhdx", FileKind::DiskImage),
    ("vmdk", FileKind::DiskImage),
    ("wav", FileKind::Audio),
    ("webm", FileKind::Video),
    ("webp", FileKind::Image),
    ("wim", FileKind::DiskImage),
    ("wma", FileKind::Audio),
    ("wmv", FileKind::Video),
    ("xls", FileKind::Document),
    ("xlsx", FileKind::Document),
    ("xml", FileKind::Code),
    ("xz", FileKind::Archive),
    ("yaml", FileKind::Code),
    ("zip", FileKind::Archive),
    ("zst", FileKind::Archive),
];
