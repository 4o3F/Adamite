use super::VolumeReader;
use crate::error::VolumeError;

/// A volume reader borrowing an immutable byte slice.
#[derive(Debug, Clone, Copy)]
pub struct SliceReader<'a> {
    bytes: &'a [u8],
}

impl<'a> SliceReader<'a> {
    /// Borrows a complete volume image starting at byte zero.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl VolumeReader for SliceReader<'_> {
    fn len_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VolumeError> {
        let out_of_bounds = || VolumeError::OutOfBounds {
            offset,
            len: buf.len(),
            volume_len: self.len_bytes(),
        };
        let start = usize::try_from(offset).map_err(|_| out_of_bounds())?;
        let end = start.checked_add(buf.len()).ok_or_else(out_of_bounds)?;
        let bytes = self.bytes.get(start..end).ok_or_else(out_of_bounds)?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}
