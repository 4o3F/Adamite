use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Format {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SnapshotParams {
    /// Saved .adm path or volume letter; omit when exactly one snapshot is discovered. For snapshots, filters the catalog.
    pub(crate) snapshot: Option<String>,
    /// Output representation: text (default) or json with matching structured content.
    #[serde(default)]
    pub(crate) format: Format,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ListDirParams {
    /// Saved .adm path or volume letter; omit when exactly one snapshot is discovered.
    pub(crate) snapshot: Option<String>,
    /// Output representation: text (default) or json with matching structured content.
    #[serde(default)]
    pub(crate) format: Format,
    /// Volume-relative directory path, default /. Use / separators and #id to disambiguate children.
    #[serde(default = "ListDirParams::root")]
    pub(crate) path: String,
    /// Number of children to skip, default 0. Offsets past the end return an empty page.
    #[serde(default)]
    pub(crate) offset: usize,
    /// Maximum children per page, default 50; allowed range 1 through 500.
    #[serde(default = "ListDirParams::limit")]
    pub(crate) limit: i64,
}

impl ListDirParams {
    fn root() -> String {
        "/".into()
    }

    const fn limit() -> i64 {
        50
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct TreeParams {
    #[serde(flatten)]
    pub(crate) common: SnapshotParams,
    /// Volume-relative directory, default /.
    #[serde(default = "ListDirParams::root")]
    pub(crate) path: String,
    /// Maximum rows including collapsed groups, 1 through 400; default 60.
    #[serde(default = "TreeParams::budget")]
    pub(crate) budget: i64,
    /// Minimum allocation percentage, 0 through 100; default 0.5.
    #[serde(default = "TreeParams::share")]
    pub(crate) min_share_percent: f64,
}

impl TreeParams {
    const fn budget() -> i64 {
        60
    }
    const fn share() -> f64 {
        0.5
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct TopParams {
    #[serde(flatten)]
    pub(crate) common: SnapshotParams,
    /// Maximum files, 1 through 100; default 20.
    #[serde(default = "TopParams::n")]
    pub(crate) n: i64,
    /// Directory subtree, default the entire volume.
    pub(crate) under: Option<String>,
    /// Extensions with or without leading dots; mutually exclusive with kind.
    pub(crate) ext: Option<Vec<String>>,
    /// FileKind name: video, audio, image, archive, disk-image, document, code, executable, other.
    pub(crate) kind: Option<String>,
}
impl TopParams {
    const fn n() -> i64 {
        20
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GroupBy {
    #[default]
    Extension,
    Kind,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct BreakdownParams {
    #[serde(flatten)]
    pub(crate) common: SnapshotParams,
    /// Maximum groups, 1 through 200; default 20.
    #[serde(default = "TopParams::n")]
    pub(crate) n: i64,
    /// Directory subtree, default the entire volume.
    pub(crate) under: Option<String>,
    /// Group by extension (default) or kind.
    #[serde(default)]
    pub(crate) by: GroupBy,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SearchParams {
    #[serde(flatten)]
    pub(crate) common: SnapshotParams,
    /// Case-insensitive name glob, or substring without glob characters; at most 256 bytes.
    pub(crate) pattern: String,
    /// Directory subtree, default /.
    pub(crate) under: Option<String>,
    /// Matches to skip; offsets past the end return an empty page.
    #[serde(default)]
    pub(crate) offset: usize,
    /// Maximum matches, 1 through 500; default 50.
    #[serde(default = "ListDirParams::limit")]
    pub(crate) limit: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct NodeParams {
    #[serde(flatten)]
    pub(crate) common: SnapshotParams,
    /// Volume-relative file or directory path; #id selects a direct child.
    pub(crate) path: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Profile {
    Background,
    #[default]
    Balanced,
    Max,
}

impl From<Profile> for adamite_io::limits::ScanProfile {
    fn from(value: Profile) -> Self {
        match value {
            Profile::Background => Self::Background,
            Profile::Balanced => Self::Balanced,
            Profile::Max => Self::Max,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ScanParams {
    /// Single ASCII drive letter with optional colon (Windows), or an existing NTFS image file.
    pub(crate) volume: String,
    /// Resource profile: background, balanced (default), or max.
    #[serde(default)]
    pub(crate) profile: Profile,
    /// Output representation: text (default) or json with matching structured content.
    #[serde(default)]
    pub(crate) format: Format,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ScanStatusParams {
    /// Current or last scan's drive letter or image path; a mismatch is invalid_argument.
    pub(crate) volume: Option<String>,
    /// Output representation: text (default) or json with matching structured content.
    #[serde(default)]
    pub(crate) format: Format,
}
