mod ffi;

use super::{VolumeReader, scan_clock::ScanClock};
use crate::{
    align::{AlignedBuf, Span},
    error::VolumeError,
};
use adamite_core::scan::ScanContext;
use std::{
    fs::{File, OpenOptions},
    os::windows::fs::{FileExt, OpenOptionsExt},
    path::PathBuf,
};
use windows::Win32::{
    Foundation::ERROR_ACCESS_DENIED,
    Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_SHARE_READ, FILE_SHARE_WRITE},
};

/// A read-only Windows volume using one aligned, unbuffered transfer per request.
///
/// A short native transfer returns [`VolumeError::ShortRead`] with the original request's offset and length. Its `actual` count includes only requested bytes covered by the transfer, excluding alignment padding; a short transfer fails even when it covers every requested byte.
#[derive(Debug)]
pub struct RawVolumeReader {
    file: File,
    len: u64,
    logical: u32,
    physical: u32,
    root: ffi::VolumeRoot,
    buf: AlignedBuf,
}

impl RawVolumeReader {
    /// Opens an ASCII drive letter and queries its volume length and sector alignment.
    ///
    /// # Errors
    ///
    /// Returns [`VolumeError::InvalidDrive`] for any character other than an ASCII letter, [`VolumeError::AccessDenied`] when access is denied, or [`VolumeError::Open`] for another open failure. Host query failures return [`VolumeError::Query`]; a zero or negative reported length is invalid data for the `"volume length"` query. Invalid sector sizes return [`VolumeError::UnsupportedAlignment`], and a partial final sector returns [`VolumeError::UnalignedVolume`].
    pub fn open_drive(letter: char) -> Result<Self, VolumeError> {
        if !letter.is_ascii_alphabetic() {
            return Err(VolumeError::InvalidDrive { letter });
        }
        let letter = letter.to_ascii_uppercase();
        let path = PathBuf::from(format!(r"\\.\{letter}:"));
        let file = OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
            .custom_flags(FILE_FLAG_NO_BUFFERING.0)
            .open(&path)
            .map_err(|source| {
                if source.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) {
                    VolumeError::AccessDenied { path, source }
                } else {
                    VolumeError::Open { path, source }
                }
            })?;
        let handle = ffi::VolumeHandle::new(&file);
        handle
            .allow_extended_dasd()
            .map_err(|source| VolumeError::Query {
                operation: "allow extended DASD I/O",
                source,
            })?;
        let len = handle.length().map_err(|source| VolumeError::Query {
            operation: "volume length",
            source,
        })?;
        let (logical, physical) =
            handle
                .access_alignment()
                .map_err(|source| VolumeError::Query {
                    operation: "access alignment",
                    source,
                })?;
        if !logical.is_power_of_two() || !physical.is_power_of_two() || physical < logical {
            return Err(VolumeError::UnsupportedAlignment { logical, physical });
        }
        if !len.is_multiple_of(u64::from(logical)) {
            return Err(VolumeError::UnalignedVolume {
                volume_len: len,
                logical,
            });
        }
        Ok(Self {
            file,
            len,
            logical,
            physical,
            root: ffi::VolumeRoot::new(letter),
            buf: AlignedBuf::default(),
        })
    }

    /// Queries the volume label, total free bytes and current scan timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`VolumeError::Query`] when a label or free-space query fails, or [`VolumeError::Overflow`] when the clock cannot be represented as FILETIME ticks.
    pub fn context(&self) -> Result<ScanContext, VolumeError> {
        let label = self.root.label().map_err(|source| VolumeError::Query {
            operation: "volume label",
            source,
        })?;
        let free_bytes = self
            .root
            .free_bytes()
            .map_err(|source| VolumeError::Query {
                operation: "volume free bytes",
                source,
            })?;
        let scanned_at_filetime = ScanClock::now_filetime()?;
        Ok(ScanContext {
            label: (!label.is_empty()).then_some(label),
            scanned_at_filetime: Some(scanned_at_filetime),
            free_bytes: Some(free_bytes),
        })
    }
}

impl VolumeReader for RawVolumeReader {
    fn len_bytes(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VolumeError> {
        let span = Span::new(offset, buf.len(), self.logical, self.len)?;
        if buf.is_empty() {
            return Ok(());
        }
        let native = self.buf.slice_mut(span.native_len(), self.physical)?;
        let read = self
            .file
            .seek_read(native, span.native_offset())
            .map_err(|source| VolumeError::Read {
                offset,
                len: buf.len(),
                source,
            })?;
        let range = span.request_range();
        if read != native.len() {
            return Err(VolumeError::ShortRead {
                offset,
                expected: buf.len(),
                actual: read.saturating_sub(range.start).min(buf.len()),
            });
        }
        let requested = native.get(range).ok_or(VolumeError::Overflow {
            operation: "raw read slice bounds",
        })?;
        buf.copy_from_slice(requested);
        Ok(())
    }
}
