use super::VolumeReader;
use crate::error::VolumeError;
use std::{fs::File, io, path::Path};

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

/// A read-only volume image starting at byte zero, with its length cached at open.
#[derive(Debug)]
pub struct ImageReader {
    file: File,
    len: u64,
}

impl ImageReader {
    /// Opens an uncompressed volume image and reads its length from the opened handle.
    ///
    /// # Errors
    ///
    /// Returns [`VolumeError::Open`] if opening fails, or [`VolumeError::Query`] with operation `"image length"` if querying the handle's metadata fails.
    pub fn open(path: &Path) -> Result<Self, VolumeError> {
        let file = File::open(path).map_err(|source| VolumeError::Open {
            path: path.to_owned(),
            source,
        })?;
        let len = file
            .metadata()
            .map_err(|source| VolumeError::Query {
                operation: "image length",
                source,
            })?
            .len();
        Ok(Self { file, len })
    }
}

impl VolumeReader for ImageReader {
    fn len_bytes(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VolumeError> {
        let end = u64::try_from(buf.len())
            .ok()
            .and_then(|len| offset.checked_add(len));
        if end.is_none_or(|end| end > self.len) {
            return Err(VolumeError::OutOfBounds {
                offset,
                len: buf.len(),
                volume_len: self.len,
            });
        }
        #[cfg(unix)]
        let read = |offset, buf: &mut [u8]| self.file.read_at(buf, offset);
        #[cfg(windows)]
        let read = |offset, buf: &mut [u8]| self.file.seek_read(buf, offset);
        ExactRead { read }.fill(offset, buf)
    }
}

struct ExactRead<F> {
    read: F,
}

impl<F> ExactRead<F>
where
    F: FnMut(u64, &mut [u8]) -> io::Result<usize>,
{
    fn fill(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VolumeError> {
        let expected = buf.len();
        let mut actual = 0usize;
        let mut position = offset;
        let mut remaining = buf;
        while !remaining.is_empty() {
            match (self.read)(position, remaining) {
                Ok(0) => {
                    return Err(VolumeError::ShortRead {
                        offset,
                        expected,
                        actual,
                    });
                }
                Ok(read) => {
                    remaining = remaining.get_mut(read..).ok_or_else(|| VolumeError::Read {
                        offset,
                        len: expected,
                        source: io::Error::new(
                            io::ErrorKind::InvalidData,
                            "positional read exceeded buffer length",
                        ),
                    })?;
                    actual = actual.checked_add(read).ok_or(VolumeError::Overflow {
                        operation: "image read count",
                    })?;
                    position = u64::try_from(read)
                        .ok()
                        .and_then(|read| position.checked_add(read))
                        .ok_or(VolumeError::Overflow {
                            operation: "image read offset",
                        })?;
                }
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) => {
                    return Err(VolumeError::Read {
                        offset,
                        len: expected,
                        source,
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ExactRead;
    use crate::error::VolumeError;
    use std::{error::Error, io};

    #[test]
    fn interrupted_then_partial_reads_fill_the_buffer_at_advancing_offsets()
    -> Result<(), Box<dyn Error>> {
        let mut steps = [
            Err(io::Error::from(io::ErrorKind::Interrupted)),
            Ok(2),
            Ok(3),
        ]
        .into_iter();
        let mut offsets = Vec::new();
        let mut lengths = Vec::new();
        let mut buffer = [0; 5];
        ExactRead {
            read: |offset, buf: &mut [u8]| {
                offsets.push(offset);
                lengths.push(buf.len());
                let count = steps
                    .next()
                    .ok_or_else(|| io::Error::other("unexpected read"))??;
                buf.get_mut(..count)
                    .ok_or_else(|| io::Error::other("invalid scripted length"))?
                    .fill(u8::try_from(offset).map_err(io::Error::other)?);
                Ok(count)
            },
        }
        .fill(16, &mut buffer)?;
        assert_eq!(offsets, [16, 16, 18]);
        assert_eq!(lengths, [5, 5, 3]);
        assert_eq!(buffer, [16, 16, 18, 18, 18]);
        assert!(steps.next().is_none());
        Ok(())
    }

    #[test]
    fn partial_then_zero_reports_original_range_and_actual_count() -> Result<(), Box<dyn Error>> {
        let mut steps = [2, 0].into_iter();
        let mut offsets = Vec::new();
        let error = ExactRead {
            read: |offset, _: &mut [u8]| {
                offsets.push(offset);
                steps
                    .next()
                    .ok_or_else(|| io::Error::other("unexpected read"))
            },
        }
        .fill(16, &mut [0; 5])
        .err()
        .ok_or("short read accepted")?;
        assert!(matches!(
            error,
            VolumeError::ShortRead {
                offset: 16,
                expected: 5,
                actual: 2,
            }
        ));
        assert_eq!(offsets, [16, 18]);
        assert!(error.source().is_none());
        assert!(steps.next().is_none());
        Ok(())
    }

    #[test]
    fn partial_then_error_preserves_original_range_and_host_source() -> Result<(), Box<dyn Error>> {
        let source = io::Error::from_raw_os_error(5);
        let kind = source.kind();
        let mut steps = [Ok(2), Err(source)].into_iter();
        let mut offsets = Vec::new();
        let error = ExactRead {
            read: |offset, _: &mut [u8]| {
                offsets.push(offset);
                steps
                    .next()
                    .ok_or_else(|| io::Error::other("unexpected read"))?
            },
        }
        .fill(16, &mut [0; 5])
        .err()
        .ok_or("read failure ignored")?;
        assert!(matches!(
            error,
            VolumeError::Read {
                offset: 16,
                len: 5,
                ..
            }
        ));
        assert_eq!(offsets, [16, 18]);
        let source = error.source().ok_or("read source was lost")?;
        let source = source
            .downcast_ref::<io::Error>()
            .ok_or("wrong read source type")?;
        assert_eq!(source.raw_os_error(), Some(5));
        assert_eq!(source.kind(), kind);
        assert!(source.source().is_none());
        assert!(steps.next().is_none());
        Ok(())
    }
}
