//! Shared bounded file adapters for canonical snapshot persistence.

use adamite_core::{
    scan::ScanOutput,
    snapshot::{PersistError, SnapshotDecoder},
};
use snafu::Snafu;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};

/// Read block size; the two passes reuse one buffer of this length.
const BLOCK_SIZE: usize = 1 << 20;

/// File access or codec failure while reading or writing a snapshot.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum SnapshotIoError {
    /// Host file operation failed.
    #[snafu(display("snapshot I/O failed: {source}"))]
    Io {
        /// Underlying file error.
        source: io::Error,
    },
    /// Canonical format validation or encoding failed.
    #[snafu(display("{source}"))]
    Persist {
        /// Underlying codec error.
        source: PersistError,
    },
    /// The file length cannot be represented on this host.
    #[snafu(display("snapshot length exceeds the host address space"))]
    Length,
}

impl From<PersistError> for SnapshotIoError {
    fn from(source: PersistError) -> Self {
        Self::Persist { source }
    }
}

impl From<io::Error> for SnapshotIoError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

/// Writes chunks and patches the CRC in an already opened, empty file.
#[derive(Debug)]
pub struct SnapshotWriter;

impl SnapshotWriter {
    /// Writes from file offset zero, leaving the cursor at the encoded file end.
    ///
    /// The caller owns file creation, truncation, synchronization and publication.
    ///
    /// # Errors
    /// Returns codec, seek or write errors.
    pub fn write(
        output: &ScanOutput,
        target: &mut (impl Write + Seek),
    ) -> Result<usize, SnapshotIoError> {
        let _entered =
            tracing::info_span!("persist_encode", nodes = output.snapshot.len()).entered();
        let mut encoder = output.encoder()?;
        target.rewind()?;
        while let Some(chunk) = encoder.next_chunk()? {
            target.write_all(chunk)?;
        }
        if let Some((offset, crc)) = encoder.checksum_patch() {
            let offset = u64::try_from(offset).map_err(|_| SnapshotIoError::Length)?;
            target.seek(SeekFrom::Start(offset))?;
            target.write_all(&crc)?;
        }
        let len = encoder.file_len();
        target.seek(SeekFrom::Start(
            u64::try_from(len).map_err(|_| SnapshotIoError::Length)?,
        ))?;
        Ok(len)
    }
}

/// Verifies and decodes two sequential passes over the same seekable file.
#[derive(Debug)]
pub struct SnapshotReader;

impl SnapshotReader {
    /// Reads final snapshot columns with a fixed 1 MiB file buffer.
    ///
    /// The source must remain unchanged for both passes.
    ///
    /// # Errors
    /// Returns file access, allocation or persistence validation errors.
    pub fn read(source: &mut (impl Read + Seek)) -> Result<ScanOutput, SnapshotIoError> {
        let _entered = tracing::info_span!("persist_decode").entered();
        let len =
            usize::try_from(source.seek(SeekFrom::End(0))?).map_err(|_| SnapshotIoError::Length)?;
        source.rewind()?;
        let mut blocks = BufReader::with_capacity(BLOCK_SIZE, source);
        let decoder = SnapshotDecoder::verify(len, |target: &mut [u8]| {
            blocks.read_exact(target).map_err(SnapshotIoError::from)
        })?;
        blocks.rewind()?;
        let (snapshot, stats) = decoder
            .decode(|target: &mut [u8]| blocks.read_exact(target).map_err(SnapshotIoError::from))?;
        Ok(ScanOutput { snapshot, stats })
    }
}
