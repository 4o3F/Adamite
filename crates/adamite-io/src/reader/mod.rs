//! Exact positional reads from a source of known length.

mod image;
mod scan_clock;
mod slice;
#[cfg(windows)]
mod windows;

pub use image::ImageReader;
pub use slice::SliceReader;
#[cfg(windows)]
pub use windows::RawVolumeReader;

use crate::error::VolumeError;

/// A volume-relative source whose reads fill the entire caller-owned buffer.
///
/// A successful read supplies exactly the requested bytes without zero-filling or returning partial data. Implementations check bounds before I/O and return [`VolumeError::OutOfBounds`] if the range overflows or exceeds [`Self::len_bytes`]. A failed read may modify the buffer, but its contents must not be consumed as a successful response.
pub trait VolumeReader {
    /// Returns the source length in bytes.
    fn len_bytes(&self) -> u64;

    /// Fills `buf` from the volume-relative range starting at `offset`.
    ///
    /// # Errors
    ///
    /// Returns [`VolumeError::OutOfBounds`] before I/O for an invalid range, or a read failure when the complete range cannot be supplied.
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VolumeError>;
}
