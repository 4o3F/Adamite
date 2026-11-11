use crate::{CountingAllocator, SpanRecorder, SpanSummary};
use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::Instant,
};
use tracing::{
    Subscriber,
    span::{Attributes, Id},
};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

/// Adds a `heap_usage` event immediately before each span closes.
///
/// Each span's peak covers its entire open lifetime, including nested or overlapping spans.
/// Measurements include all threads and profiling overhead. This layer consumes the allocator's
/// interval peaks; callers must not consume them separately while the layer is active.
#[derive(Debug)]
pub struct HeapLayer {
    allocator: &'static CountingAllocator,
    state: Mutex<HeapState>,
    recorder: Option<SpanRecorder>,
}

#[derive(Debug)]
struct SpanHeap {
    name: &'static str,
    started: Instant,
    start_current: usize,
    running_max: usize,
}

#[derive(Debug, Default)]
struct HeapState {
    open: HashMap<Id, SpanHeap>,
    process_peak: usize,
}

impl HeapState {
    fn boundary(&mut self, allocator: &CountingAllocator) -> usize {
        let peak = allocator.take_peak();
        let current = allocator.current();
        let peak = peak.max(current);
        self.process_peak = self.process_peak.max(peak);
        for span in self.open.values_mut() {
            span.running_max = span.running_max.max(peak);
        }
        current
    }
}

impl HeapLayer {
    /// Uses the same allocator instance registered by the executable.
    #[must_use]
    pub fn new(allocator: &'static CountingAllocator) -> Self {
        Self {
            allocator,
            state: Mutex::new(HeapState::default()),
            recorder: None,
        }
    }

    pub(crate) fn recording(allocator: &'static CountingAllocator, recorder: SpanRecorder) -> Self {
        Self {
            recorder: Some(recorder),
            ..Self::new(allocator)
        }
    }
}

impl<S> Layer<S> for HeapLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, _context: Context<'_, S>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let current = state.boundary(self.allocator);
        state.open.insert(
            id.clone(),
            SpanHeap {
                name: attributes.metadata().name(),
                started: Instant::now(),
                start_current: current,
                running_max: current,
            },
        );
    }

    fn on_close(&self, id: Id, context: Context<'_, S>) {
        let measurement = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let current = state.boundary(self.allocator);
            state
                .open
                .remove(&id)
                .map(|span| (current, span, state.process_peak))
        };
        let Some((current, heap, process_peak)) = measurement else {
            return;
        };
        let Some(span) = context.span(&id) else {
            return;
        };
        let delta = current as i128 - heap.start_current as i128;
        if let Some(recorder) = &self.recorder {
            recorder.record(SpanSummary {
                name: heap.name,
                duration: heap.started.elapsed(),
                heap_peak: heap.running_max,
                heap_delta: delta,
                heap_current: current,
                process_peak,
            });
        }
        tracing::event!(
            name: "heap_usage",
            parent: &id,
            tracing::Level::INFO,
            span_name = %span.metadata().name(),
            span_id = id.into_u64(),
            heap_current = current,
            heap_peak = heap.running_max,
            heap_delta = delta,
            process_peak,
        );
    }
}
