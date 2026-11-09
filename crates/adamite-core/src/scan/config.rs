//! Caller metadata and validated transport bounds.

use snafu::Snafu;

/// Smallest configurable chunk and the boot-sector request size, in bytes.
pub(super) const BOOT_BYTES: usize = 512;
/// Largest configurable chunk, in bytes.
const MAX_CHUNK_BYTES: usize = 16_777_216;
/// Default configurable chunk, in bytes.
const DEFAULT_CHUNK_BYTES: usize = 1_048_576;

/// Optional metadata acquired by the caller rather than by the scanner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanContext {
    /// Caller-provided volume label.
    pub label: Option<String>,
    /// Caller-provided scan timestamp as a raw FILETIME.
    pub scanned_at_filetime: Option<u64>,
    /// Caller-provided free space in bytes.
    pub free_bytes: Option<u64>,
}

/// Transport bounds and caller metadata for a single scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanConfig {
    /// Maximum issued but unconsumed requests, including responses already fed.
    pub max_outstanding: usize,
    /// Maximum request length; a multiple of 512 between 512 bytes and 16 MiB.
    pub chunk_size: usize,
    /// Optional caller-provided metadata.
    pub context: ScanContext,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            max_outstanding: 1,
            chunk_size: DEFAULT_CHUNK_BYTES,
            context: ScanContext::default(),
        }
    }
}

impl ScanConfig {
    /// Validates request capacity, chunk size and their product without allocating.
    ///
    /// # Errors
    ///
    /// Rejects zero capacity, invalid chunk sizes and an overflowing capacity product.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_outstanding == 0 {
            return Err(ConfigError::ZeroOutstanding);
        }
        if !(BOOT_BYTES..=MAX_CHUNK_BYTES).contains(&self.chunk_size)
            || !self.chunk_size.is_multiple_of(BOOT_BYTES)
        {
            return Err(ConfigError::InvalidChunkSize {
                chunk_size: self.chunk_size,
            });
        }
        if self.max_outstanding.checked_mul(self.chunk_size).is_none() {
            return Err(ConfigError::BufferSizeOverflow {
                max_outstanding: self.max_outstanding,
                chunk_size: self.chunk_size,
            });
        }
        Ok(())
    }
}

/// Invalid scanner configuration.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum ConfigError {
    /// No request slots were permitted.
    #[snafu(display("outstanding request capacity must be positive"))]
    ZeroOutstanding,
    /// The chunk size is outside the supported aligned range.
    #[snafu(display("invalid chunk size {chunk_size}"))]
    InvalidChunkSize {
        /// Rejected byte count.
        chunk_size: usize,
    },
    /// The configured transport byte bound does not fit usize.
    #[snafu(display("request capacity {max_outstanding} times chunk size {chunk_size} overflows"))]
    BufferSizeOverflow {
        /// Requested slot capacity.
        max_outstanding: usize,
        /// Requested chunk size.
        chunk_size: usize,
    },
}
