//! Fused attribute iteration over a bounded record region.

use core::iter::FusedIterator;

use snafu::OptionExt;

use super::{
    Attribute, AttributeError, AttributePastUsedBytesSnafu, InvalidRegionSnafu,
    MissingEndMarkerSnafu,
};
use crate::util;

/// Fused attribute iterator bounded to the record's declared used-byte region.
/// Exhaust the iterator to validate every header and the end marker.
#[must_use = "exhaust the attribute iterator to validate the complete record"]
#[derive(Debug, Clone)]
pub struct AttributeIter<'a> {
    region: Option<&'a [u8]>,
    cursor: usize,
    finished: bool,
}

impl<'a> AttributeIter<'a> {
    /// Borrows an arbitrary attribute region in instrumented fuzz builds.
    #[cfg(fuzzing)]
    pub fn from_region(region: &'a [u8]) -> Self {
        Self::new(Some(region))
    }

    pub(crate) fn new(region: Option<&'a [u8]>) -> Self {
        Self {
            region,
            cursor: 0,
            finished: false,
        }
    }

    fn read_next(&mut self) -> Result<Option<Attribute<'a>>, AttributeError> {
        let input = self
            .region
            .context(InvalidRegionSnafu)?
            .get(self.cursor..)
            .context(InvalidRegionSnafu)?;
        let kind = util::read_u32(input, 0).context(MissingEndMarkerSnafu)?;
        if kind == u32::MAX {
            return Ok(None);
        }
        let attribute = Attribute::parse(input)?;
        self.cursor = util::range(self.cursor, attribute.bytes.len())
            .context(AttributePastUsedBytesSnafu)?
            .end;
        Ok(Some(attribute))
    }
}

impl<'a> Iterator for AttributeIter<'a> {
    type Item = Result<Attribute<'a>, AttributeError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let step = self.read_next();
        crate::util::finish_step(step, &mut self.finished)
    }
}
impl FusedIterator for AttributeIter<'_> {}
