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
