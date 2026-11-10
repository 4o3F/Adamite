//! Identified request slots consumed in the planner's virtual order.

use crate::error::ScanError;
use snafu::Snafu;
use std::collections::{TryReserveError, VecDeque};

/// An opaque request identifier unique within one scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

impl RequestId {
    /// Restores an identifier from its transport representation without validating liveness.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the transport representation.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// A nonempty volume-relative byte range to be supplied by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadRequest {
    /// Identifier passed back to Scanner::feed.
    pub id: RequestId,
    /// Physical byte offset relative to the volume start.
    pub offset: u64,
    /// Exact number of bytes required.
    pub len: usize,
}

/// A rejected response, leaving pending requests and accepted bytes unchanged.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
#[non_exhaustive]
pub enum FeedError {
    /// The scan has reached a terminal state.
    #[snafu(display("scan is inactive"))]
    Inactive,
    /// The identifier was never issued or has already been consumed.
    #[snafu(display("unknown or consumed request {id:?}"))]
    UnknownRequest {
        /// Rejected identifier.
        id: RequestId,
    },
    /// This pending slot already holds an accepted response.
    #[snafu(display("request {id:?} already has a response"))]
    DuplicateResponse {
        /// Identifier of the occupied response slot.
        id: RequestId,
    },
    /// Responses must contain the complete requested range.
    #[snafu(display("request {id:?} needs {expected} bytes, received {actual}"))]
    LengthMismatch {
        /// Rejected identifier.
        id: RequestId,
        /// Required byte count.
        expected: usize,
        /// Supplied byte count.
        actual: usize,
    },
    /// Owned response storage could not be allocated.
    #[snafu(display("response allocation failed: {source}"))]
    Allocation {
        /// Allocation failure reported by Vec.
        source: TryReserveError,
    },
}

#[derive(Debug)]
#[cfg_attr(test, derive(Clone, PartialEq, Eq))]
struct Slot {
    request: ReadRequest,
    response: Option<Vec<u8>>,
    consumed: usize,
}

/// The planner enqueues ranges in virtual order, independent of physical offsets.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone, PartialEq, Eq))]
pub(super) struct Requests {
    slots: VecDeque<Slot>,
    max_outstanding: usize,
    chunk_size: usize,
    next_id: Option<u64>,
    active: bool,
}

impl Requests {
    pub(super) fn new(max_outstanding: usize, chunk_size: usize) -> Self {
        Self {
            slots: VecDeque::new(),
            max_outstanding,
            chunk_size,
            next_id: Some(0),
            active: true,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(super) fn issue(&mut self, offset: u64, len: usize) -> Result<ReadRequest, ScanError> {
        if !self.active {
            return Err(ScanError::Inactive);
        }
        if self.slots.len() >= self.max_outstanding {
            return Err(ScanError::OutstandingLimit {
                max_outstanding: self.max_outstanding,
            });
        }
        if len == 0 || len > self.chunk_size {
            return Err(ScanError::InvalidReadRange { offset, len });
        }
        let length = u64::try_from(len).ok();
        if length
            .and_then(|length| offset.checked_add(length))
            .is_none()
        {
            return Err(ScanError::InvalidReadRange { offset, len });
        }
        let id = self.next_id.ok_or(ScanError::RequestIdExhausted)?;
        self.slots.try_reserve(1)?;
        let request = ReadRequest {
            id: RequestId(id),
            offset,
            len,
        };
        self.slots.push_back(Slot {
            request,
            response: None,
            consumed: 0,
        });
        self.next_id = id.checked_add(1);
        Ok(request)
    }

    pub(super) fn feed(&mut self, id: RequestId, bytes: &[u8]) -> Result<(), FeedError> {
        if !self.active {
            return Err(FeedError::Inactive);
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.request.id == id)
            .ok_or(FeedError::UnknownRequest { id })?;
        if slot.response.is_some() {
            return Err(FeedError::DuplicateResponse { id });
        }
        if bytes.len() != slot.request.len {
            return Err(FeedError::LengthMismatch {
                id,
                expected: slot.request.len,
                actual: bytes.len(),
            });
        }
        let mut response = Vec::new();
        response
            .try_reserve_exact(bytes.len())
            .map_err(|source| FeedError::Allocation { source })?;
        response.extend_from_slice(bytes);
        slot.response = Some(response);
        Ok(())
    }

    pub(super) fn has_ready(&self) -> bool {
        self.slots
            .front()
            .is_some_and(|slot| slot.response.is_some())
    }

    pub(super) fn take_ready(&mut self) -> Option<(ReadRequest, Vec<u8>)> {
        self.slots.front()?.response.as_ref()?;
        let slot = self.slots.pop_front()?;
        slot.response.map(|response| (slot.request, response))
    }

    /// Keeps a partially consumed response in its slot until its last byte is used.
    pub(super) fn ready_bytes(&self) -> Option<&[u8]> {
        let slot = self.slots.front()?;
        slot.response.as_ref()?.get(slot.consumed..)
    }

    pub(super) fn consume_prefix(&mut self, len: usize) -> Result<(), ScanError> {
        let slot = self.slots.front_mut().ok_or(ScanError::Stream {
            reason: "missing stream response",
        })?;
        let response = slot.response.as_ref().ok_or(ScanError::Stream {
            reason: "stream response not ready",
        })?;
        let end = slot.consumed.checked_add(len).ok_or(ScanError::Overflow {
            operation: "response consumption cursor",
        })?;
        if end > response.len() {
            return Err(ScanError::Stream {
                reason: "stream response over-consumed",
            });
        }
        slot.consumed = end;
        if end == response.len() {
            self.slots.pop_front();
        }
        Ok(())
    }

    pub(super) fn retained_bytes(&self) -> Result<usize, ScanError> {
        self.slots.iter().try_fold(0usize, |total, slot| {
            total
                .checked_add(slot.response.as_ref().map_or(0, Vec::capacity))
                .ok_or(ScanError::Overflow {
                    operation: "retained transport bytes",
                })
        })
    }

    pub(super) fn deactivate(&mut self) {
        self.active = false;
        self.slots.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{FeedError, RequestId, Requests, ScanError};
    use proptest::prelude::*;
    use std::error::Error;

    #[test]
    fn reordered_responses_wait_for_virtual_order_and_hold_capacity() -> Result<(), Box<dyn Error>>
    {
        let mut requests = Requests::new(3, 512);
        // Enqueue virtual pieces whose physical offsets are deliberately unordered.
        let first = requests.issue(8192, 2)?;
        let second = requests.issue(512, 2)?;
        let third = requests.issue(4096, 2)?;
        assert!(first.id < second.id && second.id < third.id);
        requests.feed(third.id, &[3, 3])?;
        requests.feed(second.id, &[2, 2])?;
        assert_eq!(requests.take_ready(), None);
        let before = requests.clone();
        assert!(matches!(
            requests.issue(0, 1),
            Err(ScanError::OutstandingLimit { max_outstanding: 3 })
        ));
        assert_eq!(requests, before);
        requests.feed(first.id, &[1, 1])?;
        assert_eq!(requests.len(), 3);
        assert_eq!(requests.take_ready(), Some((first, vec![1, 1])));
        let fourth = requests.issue(0, 1)?;
        assert!(fourth.id > third.id);
        assert_eq!(requests.take_ready(), Some((second, vec![2, 2])));
        assert_eq!(requests.take_ready(), Some((third, vec![3, 3])));
        assert_eq!(requests.take_ready(), None);
        requests.feed(fourth.id, &[4])?;
        assert_eq!(requests.take_ready(), Some((fourth, vec![4])));
        assert_eq!(requests.len(), 0);
        Ok(())
    }

    #[test]
    fn rejected_feeds_are_transactional_and_responses_are_owned() -> Result<(), Box<dyn Error>> {
        let mut requests = Requests::new(2, 512);
        let first = requests.issue(512, 2)?;
        let second = requests.issue(0, 1)?;
        for bytes in [vec![], vec![1], vec![1, 2, 3]] {
            let before = requests.clone();
            assert!(matches!(
                requests.feed(first.id, &bytes),
                Err(FeedError::LengthMismatch { .. })
            ));
            assert_eq!(requests, before);
        }
        let before = requests.clone();
        assert!(matches!(
            requests.feed(RequestId::from_raw(u64::MAX), &[0]),
            Err(FeedError::UnknownRequest { .. })
        ));
        assert_eq!(requests, before);
        let mut caller_bytes = vec![7, 8];
        requests.feed(first.id, &caller_bytes)?;
        caller_bytes.fill(0);
        drop(caller_bytes);
        let before = requests.clone();
        assert!(matches!(
            requests.feed(first.id, &[9, 9]),
            Err(FeedError::DuplicateResponse { .. })
        ));
        assert_eq!(requests, before);
        assert_eq!(requests.take_ready(), Some((first, vec![7, 8])));
        let before = requests.clone();
        assert!(matches!(
            requests.feed(first.id, &[7, 8]),
            Err(FeedError::UnknownRequest { .. })
        ));
        assert_eq!(requests, before);
        requests.feed(second.id, &[4])?;
        assert_eq!(requests.take_ready(), Some((second, vec![4])));
        Ok(())
    }

    #[test]
    fn invalid_ranges_and_inactive_requests_do_not_change_state() -> Result<(), Box<dyn Error>> {
        let mut requests = Requests::new(2, 512);
        for (offset, len) in [(0, 0), (0, 513), (u64::MAX, 1)] {
            let before = requests.clone();
            assert!(matches!(
                requests.issue(offset, len),
                Err(ScanError::InvalidReadRange { .. })
            ));
            assert_eq!(requests, before);
        }
        let request = requests.issue(0, 1)?;
        requests.deactivate();
        let before = requests.clone();
        assert!(matches!(requests.issue(0, 1), Err(ScanError::Inactive)));
        assert!(matches!(
            requests.feed(request.id, &[0]),
            Err(FeedError::Inactive)
        ));
        assert_eq!(requests.take_ready(), None);
        assert_eq!(requests, before);
        Ok(())
    }

    #[test]
    fn identifiers_never_wrap_or_reuse_consumed_slots() -> Result<(), Box<dyn Error>> {
        let mut requests = Requests::new(2, 512);
        requests.next_id = Some(u64::MAX);
        let last = requests.issue(0, 1)?;
        assert_eq!(last.id.raw(), u64::MAX);
        let before = requests.clone();
        assert!(matches!(
            requests.issue(1, 1),
            Err(ScanError::RequestIdExhausted)
        ));
        assert_eq!(requests, before);
        requests.feed(last.id, &[42])?;
        assert_eq!(requests.take_ready(), Some((last, vec![42])));
        let before = requests.clone();
        assert!(matches!(
            requests.issue(1, 1),
            Err(ScanError::RequestIdExhausted)
        ));
        assert_eq!(requests, before);
        Ok(())
    }

    proptest! {
        #[test]
        fn response_order_does_not_change_virtual_output(
            pieces in prop::collection::vec((any::<u8>(), any::<u8>()), 1..9),
        ) {
            let mut requests = Requests::new(pieces.len(), 512);
            let mut expected = Vec::new();
            let mut arrivals = Vec::new();
            for &(priority, value) in &pieces {
                let request = requests.issue(u64::from(value), 1)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                expected.push((request, vec![value]));
                arrivals.push((priority, request, value));
            }
            arrivals.sort_by_key(|&(priority, _, _)| priority);
            for (_, request, value) in arrivals {
                requests.feed(request.id, &[value])
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
            }
            prop_assert_eq!(requests.len(), pieces.len());
            prop_assert!(
                matches!(requests.issue(0, 1), Err(ScanError::OutstandingLimit { .. })),
                "fed but unconsumed responses must occupy request slots"
            );
            for piece in expected {
                prop_assert_eq!(requests.take_ready(), Some(piece));
            }
            prop_assert_eq!(requests.take_ready(), None);
            prop_assert_eq!(requests.len(), 0);
        }
    }
}
