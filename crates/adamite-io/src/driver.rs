//! Synchronous request batching and exact response delivery.

use crate::{error::VolumeError, reader::VolumeReader};
use adamite_core::scan::{
    FeedError, ReadRequest, ScanConfig, ScanOutput, ScanPhase, ScanStep, Scanner,
};
use std::collections::VecDeque;

/// Reusable synchronous scan driver with validated configuration.
#[derive(Debug, Clone)]
pub struct SyncDriver {
    config: ScanConfig,
}

impl SyncDriver {
    /// Validates configuration before creating a reusable driver.
    ///
    /// # Errors
    ///
    /// Returns [`VolumeError::Config`] when request capacity or chunk size is invalid.
    pub fn new(config: ScanConfig) -> Result<Self, VolumeError> {
        config
            .validate()
            .map_err(|source| VolumeError::Config { source })?;
        Ok(Self { config })
    }

    /// Borrows the validated configuration and caller-supplied context.
    #[must_use]
    pub fn config(&self) -> &ScanConfig {
        &self.config
    }

    /// Runs a fresh scanner to completion, serving requests in issue order.
    ///
    /// # Errors
    ///
    /// Returns read and bounds failures, allocation or arithmetic failures, wrapped core errors, or [`VolumeError::Protocol`] for inconsistent scanner steps.
    pub fn run<R: VolumeReader + ?Sized>(&self, reader: &mut R) -> Result<ScanOutput, VolumeError> {
        let mut scanner =
            Scanner::new(self.config.clone()).map_err(|source| VolumeError::Config { source })?;
        let mut pending = Pending::default();
        loop {
            let step = scanner
                .step()
                .map_err(|source| VolumeError::Scan { source })?;
            if let Some(output) = pending.handle(step, reader, |request, bytes| {
                scanner.feed(request.id, bytes)
            })? {
                return Ok(output);
            }
        }
    }
}

#[derive(Debug, Default)]
struct Pending {
    requests: VecDeque<ReadRequest>,
    buffer: Vec<u8>,
    phase: Option<ScanPhase>,
}

impl Pending {
    fn handle<R, F>(
        &mut self,
        step: ScanStep,
        reader: &mut R,
        mut feed: F,
    ) -> Result<Option<ScanOutput>, VolumeError>
    where
        R: VolumeReader + ?Sized,
        F: FnMut(ReadRequest, &[u8]) -> Result<(), FeedError>,
    {
        match step {
            ScanStep::Need(request) => {
                let volume_len = reader.len_bytes();
                let end = u64::try_from(request.len)
                    .ok()
                    .and_then(|len| request.offset.checked_add(len));
                if end.is_none_or(|end| end > volume_len) {
                    return Err(VolumeError::OutOfBounds {
                        offset: request.offset,
                        len: request.len,
                        volume_len,
                    });
                }
                self.requests
                    .try_reserve(1)
                    .map_err(|source| VolumeError::Allocation { source })?;
                self.requests.push_back(request);
            }
            ScanStep::Progress(progress) => {
                self.phase = Some(progress.phase);
                if progress.waiting_for_input {
                    if self.requests.is_empty() {
                        return Err(
                            self.protocol("waiting without pending requests", progress.outstanding)
                        );
                    }
                    self.serve(reader, &mut feed)?;
                }
            }
            ScanStep::Done(output) => {
                self.finish()?;
                return Ok(Some(output));
            }
            _ => return Err(self.protocol("unsupported scan step", self.requests.len())),
        }
        Ok(None)
    }

    fn serve<R, F>(&mut self, reader: &mut R, feed: &mut F) -> Result<(), VolumeError>
    where
        R: VolumeReader + ?Sized,
        F: FnMut(ReadRequest, &[u8]) -> Result<(), FeedError>,
    {
        let span = tracing::info_span!(
            "driver_batch",
            requests = self.requests.len(),
            bytes = tracing::field::Empty
        );
        let mut bytes = 0usize;
        while let Some(request) = self.requests.pop_front() {
            let additional = request.len.saturating_sub(self.buffer.len());
            self.buffer
                .try_reserve(additional)
                .map_err(|source| VolumeError::Allocation { source })?;
            self.buffer.resize(request.len, 0);
            {
                let _entered = span.enter();
                reader.read_exact_at(request.offset, &mut self.buffer)?;
            }
            bytes = bytes
                .checked_add(request.len)
                .ok_or(VolumeError::Overflow {
                    operation: "batch byte count",
                })?;
            feed(request, &self.buffer).map_err(|source| VolumeError::Feed { request, source })?;
        }
        span.record("bytes", bytes);
        Ok(())
    }

    fn finish(&self) -> Result<(), VolumeError> {
        if self.requests.is_empty() {
            Ok(())
        } else {
            Err(self.protocol("scan completed with pending requests", self.requests.len()))
        }
    }

    fn protocol(&self, reason: &'static str, outstanding: usize) -> VolumeError {
        VolumeError::Protocol {
            reason,
            phase: self.phase,
            outstanding,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Pending;
    use crate::{error::VolumeError, reader::VolumeReader};
    use adamite_core::scan::{ReadRequest, RequestId, ScanPhase, ScanProgress, ScanStep};
    use std::error::Error;

    #[derive(Default)]
    struct RecordingReader {
        reads: Vec<(u64, usize)>,
    }

    impl VolumeReader for RecordingReader {
        fn len_bytes(&self) -> u64 {
            4096
        }

        fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VolumeError> {
            let value = u8::try_from(offset).map_err(|_| VolumeError::Overflow {
                operation: "recording reader value",
            })?;
            self.reads.push((offset, buf.len()));
            buf.fill(value);
            Ok(())
        }
    }

    fn progress(waiting_for_input: bool, outstanding: usize) -> ScanStep {
        ScanStep::Progress(ScanProgress {
            phase: ScanPhase::Stream,
            records_processed: 0,
            outstanding,
            waiting_for_input,
        })
    }

    #[test]
    fn needs_and_cpu_progress_defer_reads_until_one_waiting_batch() -> Result<(), Box<dyn Error>> {
        let mut pending = Pending::default();
        let mut reader = RecordingReader::default();
        let requests = [
            ReadRequest {
                id: RequestId::from_raw(9),
                offset: 48,
                len: 8,
            },
            ReadRequest {
                id: RequestId::from_raw(3),
                offset: 16,
                len: 16,
            },
            ReadRequest {
                id: RequestId::from_raw(5),
                offset: 32,
                len: 4,
            },
        ];
        let mut fed = Vec::new();
        for request in requests {
            assert!(
                pending
                    .handle(ScanStep::Need(request), &mut reader, |request, bytes| {
                        fed.push((request, bytes.to_vec()));
                        Ok(())
                    })?
                    .is_none()
            );
            assert!(reader.reads.is_empty());
            assert!(fed.is_empty());
        }
        for _ in 0..3 {
            assert!(
                pending
                    .handle(progress(false, 3), &mut reader, |request, bytes| {
                        fed.push((request, bytes.to_vec()));
                        Ok(())
                    })?
                    .is_none()
            );
            assert!(reader.reads.is_empty());
            assert!(fed.is_empty());
        }
        assert!(
            pending
                .handle(progress(true, 3), &mut reader, |request, bytes| {
                    fed.push((request, bytes.to_vec()));
                    Ok(())
                })?
                .is_none()
        );
        assert_eq!(reader.reads, [(48, 8), (16, 16), (32, 4)]);
        assert_eq!(fed.len(), requests.len());
        for ((actual, bytes), expected) in fed.iter().zip(requests) {
            assert_eq!(*actual, expected);
            assert_eq!(bytes.len(), expected.len);
            assert!(bytes.iter().all(|byte| u64::from(*byte) == expected.offset));
        }
        pending.finish()?;
        Ok(())
    }

    #[test]
    fn waiting_serves_a_single_request_below_capacity() -> Result<(), Box<dyn Error>> {
        let mut pending = Pending::default();
        let mut reader = RecordingReader::default();
        let request = ReadRequest {
            id: RequestId::from_raw(0),
            offset: 0,
            len: 512,
        };
        pending.handle(ScanStep::Need(request), &mut reader, |_, _| Ok(()))?;
        let mut fed = Vec::new();
        pending.handle(progress(true, 1), &mut reader, |request, bytes| {
            fed.push((request, bytes.len()));
            Ok(())
        })?;
        assert_eq!(reader.reads, [(0, 512)]);
        assert_eq!(fed, [(request, 512)]);
        Ok(())
    }

    #[test]
    fn waiting_without_requests_preserves_progress_context() -> Result<(), Box<dyn Error>> {
        let mut pending = Pending::default();
        let mut reader = RecordingReader::default();
        let error = pending
            .handle(progress(true, 7), &mut reader, |_, _| Ok(()))
            .err()
            .ok_or("empty waiting queue accepted")?;
        assert!(matches!(
            error,
            VolumeError::Protocol {
                reason: "waiting without pending requests",
                phase: Some(ScanPhase::Stream),
                outstanding: 7,
            }
        ));
        assert!(error.source().is_none());
        assert!(reader.reads.is_empty());
        Ok(())
    }

    #[test]
    fn completion_requires_an_empty_queue() -> Result<(), Box<dyn Error>> {
        let mut pending = Pending::default();
        pending.finish()?;
        let mut reader = RecordingReader::default();
        pending.handle(progress(false, 0), &mut reader, |_, _| Ok(()))?;
        pending.handle(
            ScanStep::Need(ReadRequest {
                id: RequestId::from_raw(1),
                offset: 0,
                len: 512,
            }),
            &mut reader,
            |_, _| Ok(()),
        )?;
        let error = pending
            .finish()
            .err()
            .ok_or("pending completion accepted")?;
        assert!(matches!(
            error,
            VolumeError::Protocol {
                reason: "scan completed with pending requests",
                phase: Some(ScanPhase::Stream),
                outstanding: 1,
            }
        ));
        assert!(error.source().is_none());
        assert!(reader.reads.is_empty());
        Ok(())
    }

    #[test]
    fn invalid_and_overflowing_ranges_fail_before_reading() -> Result<(), Box<dyn Error>> {
        for offset in [4096, u64::MAX] {
            let mut pending = Pending::default();
            let mut reader = RecordingReader::default();
            let error = pending
                .handle(
                    ScanStep::Need(ReadRequest {
                        id: RequestId::from_raw(0),
                        offset,
                        len: 512,
                    }),
                    &mut reader,
                    |_, _| Ok(()),
                )
                .err()
                .ok_or("out-of-bounds request accepted")?;
            assert!(matches!(error, VolumeError::OutOfBounds {
                offset: actual, len: 512, volume_len: 4096,
            } if actual == offset));
            assert!(error.source().is_none());
            assert!(reader.reads.is_empty());
            pending.finish()?;
        }
        Ok(())
    }
}
