//! Requested-heap counters and opt-in Chrome tracing for Adamite executables.

mod allocator;
mod chrome;
mod heap;

pub use allocator::CountingAllocator;
pub use chrome::{ChromeProfile, ProfileError};
pub use heap::HeapLayer;
