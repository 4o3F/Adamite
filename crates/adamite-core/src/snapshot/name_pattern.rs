//! Compiled case-insensitive globs for matching snapshot node names.

use super::{QueryError, SnapshotError};
use globset::{GlobBuilder, GlobMatcher};

const PATTERN_LIMIT: usize = 256;

/// A compiled name glob together with the user's original pattern for display.
#[derive(Debug, Clone)]
pub struct NamePattern {
    original: String,
    matcher: GlobMatcher,
}

impl NamePattern {
    /// Compiles a case-insensitive glob that matches the whole node name.
    ///
    /// Input without `*`, `?`, `[` or `{` is wrapped in `*` for substring matching. Backslashes escape glob characters, and wildcards can match separators. The original input is limited to 256 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::PatternTooLong`] for input over the byte limit, [`QueryError::InvalidGlob`] for invalid glob syntax, or an allocation or overflow error while copying the pattern.
    pub fn glob(pattern: &str) -> Result<Self, QueryError> {
        if pattern.len() > PATTERN_LIMIT {
            return Err(QueryError::PatternTooLong {
                limit: PATTERN_LIMIT,
            });
        }
        let mut wrapped = String::new();
        let glob = if pattern.contains(['*', '?', '[', '{']) {
            pattern
        } else {
            let len = pattern
                .len()
                .checked_add(2)
                .ok_or_else(|| SnapshotError::overflow("glob pattern length"))?;
            wrapped.try_reserve_exact(len)?;
            wrapped.push('*');
            wrapped.push_str(pattern);
            wrapped.push('*');
            &wrapped
        };
        let matcher = GlobBuilder::new(glob)
            .case_insensitive(true)
            .literal_separator(false)
            .backslash_escape(true)
            .build()?
            .compile_matcher();
        let mut original = String::new();
        original.try_reserve_exact(pattern.len())?;
        original.push_str(pattern);
        Ok(Self { original, matcher })
    }

    /// Borrows the original user input, before substring wildcards were added.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.original
    }

    pub(super) fn matches(&self, name: &str) -> bool {
        self.matcher.is_match(name)
    }
}
