//! The allocator is the only unsafe boundary in this crate: it forwards System's contract.
#![allow(unsafe_code)]
#![deny(
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block
)]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

/// Counts live and peak requested bytes while delegating allocation to [`System`].
///
/// Counts exclude allocator metadata and track only allocations made through this instance.
#[derive(Debug, Default)]
pub struct CountingAllocator {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl CountingAllocator {
    /// Creates an allocator with zero counters, suitable for a global static.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    /// Reads the number of currently live requested bytes.
    #[must_use]
    pub fn current(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    /// Reads the largest live requested byte count since construction or the last [`Self::take_peak`] call.
    #[must_use]
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    /// Returns the interval peak and starts a new interval at the current live count.
    ///
    /// Concurrent allocation growth is folded into the next interval after the reset.
    /// A profiling layer should be the sole consumer so it observes every interval.
    #[must_use]
    pub fn take_peak(&self) -> usize {
        let current = self.current();
        let peak = self.peak.swap(current, Ordering::Relaxed);
        self.peak.fetch_max(self.current(), Ordering::Relaxed);
        peak.max(current)
    }

    fn allocated(&self, bytes: usize) {
        let current = self
            .current
            .fetch_add(bytes, Ordering::Relaxed)
            .wrapping_add(bytes);
        self.peak.fetch_max(current, Ordering::Relaxed);
    }
}

// SAFETY: Every allocation operation forwards the caller's pointer and layout contract to
// System. Counters use atomics, never allocate, and do not affect pointers or layouts.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller supplies the valid, nonzero allocation layout required by System.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            self.allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller supplies the valid, nonzero allocation layout required by System.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            self.allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: The caller returns a live pointer from this allocator with its original layout.
        unsafe { System.dealloc(pointer, layout) };
        self.current.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The caller supplies a live pointer, its original layout and a valid new size.
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            if new_size >= layout.size() {
                self.allocated(new_size.wrapping_sub(layout.size()));
            } else {
                self.current
                    .fetch_sub(layout.size().wrapping_sub(new_size), Ordering::Relaxed);
            }
        }
        replacement
    }
}

#[cfg(test)]
mod tests {
    use super::CountingAllocator;
    use crate::SpanRecorder;
    use std::{
        alloc::{GlobalAlloc, Layout},
        collections::HashMap,
        fmt,
        ptr::NonNull,
        sync::{Arc, Mutex},
    };
    use tracing::{
        Event, Subscriber,
        field::{Field, Visit},
    };
    use tracing_subscriber::{
        Layer, Registry,
        layer::{Context, SubscriberExt},
    };

    struct Allocation<'a> {
        allocator: &'a CountingAllocator,
        pointer: NonNull<u8>,
        layout: Layout,
    }

    impl<'a> Allocation<'a> {
        fn new(allocator: &'a CountingAllocator, bytes: usize) -> Self {
            let layout = Layout::from_size_align(bytes, 8).expect("valid test layout");
            // SAFETY: The layout has nonzero size and valid alignment.
            let pointer = unsafe { allocator.alloc(layout) };
            Self {
                allocator,
                pointer: NonNull::new(pointer).expect("test allocation succeeded"),
                layout,
            }
        }
    }

    impl Drop for Allocation<'_> {
        fn drop(&mut self) {
            // SAFETY: This pointer remains live and was allocated with this allocator and layout.
            unsafe { self.allocator.dealloc(self.pointer.as_ptr(), self.layout) };
        }
    }

    #[derive(Default)]
    struct HeapEvent {
        fields: HashMap<String, String>,
    }

    impl Visit for HeapEvent {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.fields
                .insert(field.name().to_owned(), format!("{value:?}"));
        }
    }

    #[derive(Clone, Default)]
    struct HeapEvents(Arc<Mutex<Vec<HeapEvent>>>);

    impl HeapEvents {
        fn assert_measurements(&self, expected: &[(&str, [i128; 4])]) {
            let events = self.0.lock().expect("test event lock poisoned");
            assert_eq!(events.len(), expected.len());
            for (event, (name, measurements)) in events.iter().zip(expected) {
                assert_eq!(event.fields["span_name"], *name);
                for (field, value) in ["heap_current", "heap_peak", "heap_delta", "process_peak"]
                    .into_iter()
                    .zip(measurements)
                {
                    assert_eq!(event.fields[field], value.to_string(), "{name}: {field}");
                }
            }
        }
    }

    impl<S: Subscriber> Layer<S> for HeapEvents {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            if event.metadata().name() == "heap_usage" {
                let mut values = HeapEvent::default();
                event.record(&mut values);
                self.0
                    .lock()
                    .expect("test event lock poisoned")
                    .push(values);
            }
        }
    }

    #[test]
    fn counts_allocations_reallocations_and_interval_peaks() {
        let allocator = CountingAllocator::new();
        let layout = Layout::from_size_align(64, 8).expect("valid layout");
        // SAFETY: The layout has nonzero size and valid alignment.
        let pointer = unsafe { allocator.alloc(layout) };
        assert!(!pointer.is_null());
        assert_eq!((allocator.current(), allocator.peak()), (64, 64));
        // SAFETY: The pointer is live with the original layout; 128 is a valid new size.
        let pointer = unsafe { allocator.realloc(pointer, layout, 128) };
        assert!(!pointer.is_null());
        let layout = Layout::from_size_align(128, 8).expect("valid layout");
        assert_eq!((allocator.current(), allocator.peak()), (128, 128));
        // SAFETY: The pointer is live with the updated layout; 32 is a valid new size.
        let pointer = unsafe { allocator.realloc(pointer, layout, 32) };
        assert!(!pointer.is_null());
        let layout = Layout::from_size_align(32, 8).expect("valid layout");
        assert_eq!((allocator.current(), allocator.peak()), (32, 128));
        assert_eq!(allocator.take_peak(), 128);
        assert_eq!(allocator.peak(), 32);
        // SAFETY: This live pointer was returned by allocator with this layout.
        unsafe { allocator.dealloc(pointer, layout) };
        assert_eq!(allocator.current(), 0);
        // SAFETY: The layout has nonzero size and valid alignment.
        let pointer = unsafe { allocator.alloc_zeroed(layout) };
        assert!(!pointer.is_null());
        assert_eq!((allocator.current(), allocator.peak()), (32, 32));
        // SAFETY: This live pointer was returned by allocator with this layout.
        unsafe { allocator.dealloc(pointer, layout) };
        assert_eq!(allocator.current(), 0);
    }

    #[test]
    fn nested_spans_have_exact_peaks_without_earlier_process_peak() {
        static ALLOCATOR: CountingAllocator = CountingAllocator::new();
        let events = HeapEvents::default();
        let recorder = SpanRecorder::new(&[]);
        let subscriber = Registry::default()
            .with(recorder.layer(&ALLOCATOR))
            .with(events.clone());
        // A historical peak must reach process_peak without inflating later spans.
        drop(Allocation::new(&ALLOCATOR, 1000));
        tracing::subscriber::with_default(subscriber, || {
            let outer = tracing::info_span!(parent: None, "nested_outer");
            let baseline = Allocation::new(&ALLOCATOR, 64);
            let inner = tracing::info_span!(parent: &outer, "nested_inner");
            drop(Allocation::new(&ALLOCATOR, 128));
            drop(inner);
            drop(baseline);
            drop(outer);
            let low = tracing::info_span!(parent: None, "later_low");
            drop(Allocation::new(&ALLOCATOR, 32));
            drop(low);
        });
        events.assert_measurements(&[
            ("nested_inner", [64, 192, 0, 1000]),
            ("nested_outer", [0, 192, 0, 1000]),
            ("later_low", [0, 32, 0, 1000]),
        ]);
        let summaries = recorder.take();
        assert_eq!(
            summaries
                .iter()
                .map(|span| (
                    span.name,
                    span.heap_peak,
                    span.heap_delta,
                    span.process_peak
                ))
                .collect::<Vec<_>>(),
            vec![
                ("nested_inner", 192, 0, 1000),
                ("nested_outer", 192, 0, 1000),
                ("later_low", 32, 0, 1000)
            ]
        );
    }

    #[test]
    fn overlapping_spans_have_exact_peaks_with_non_lifo_close_order() {
        static ALLOCATOR: CountingAllocator = CountingAllocator::new();
        let events = HeapEvents::default();
        let recorder = SpanRecorder::new(&[]);
        let subscriber = Registry::default()
            .with(recorder.layer(&ALLOCATOR))
            .with(events.clone());
        tracing::subscriber::with_default(subscriber, || {
            let first = tracing::info_span!(parent: None, "overlap_first");
            let baseline = Allocation::new(&ALLOCATOR, 40);
            drop(Allocation::new(&ALLOCATOR, 200));
            let second = tracing::info_span!(parent: None, "overlap_second");
            drop(Allocation::new(&ALLOCATOR, 80));
            drop(first);
            drop(baseline);
            let third = tracing::info_span!(parent: None, "overlap_third");
            drop(Allocation::new(&ALLOCATOR, 16));
            drop(second);
            drop(third);
        });
        events.assert_measurements(&[
            ("overlap_first", [40, 240, 40, 240]),
            ("overlap_second", [0, 120, -40, 240]),
            ("overlap_third", [0, 16, 0, 240]),
        ]);
        let summaries = recorder.take();
        assert_eq!(
            summaries
                .iter()
                .map(|span| (span.name, span.heap_peak, span.heap_delta))
                .collect::<Vec<_>>(),
            vec![
                ("overlap_first", 240, 40),
                ("overlap_second", 120, -40),
                ("overlap_third", 16, 0)
            ]
        );
    }
}
