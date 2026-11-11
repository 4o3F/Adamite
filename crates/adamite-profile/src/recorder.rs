use crate::{CountingAllocator, HeapLayer};
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

/// A closed span's wall-clock lifetime and process-wide requested-heap measurements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanSummary {
    /// Static tracing span name.
    pub name: &'static str,
    /// Time from span creation to closure, including time outside entered guards.
    pub duration: Duration,
    /// Largest live requested byte count during this span's lifetime.
    pub heap_peak: usize,
    /// Signed live-byte change from creation to closure.
    pub heap_delta: i128,
    /// Live requested bytes when the span closed.
    pub heap_current: usize,
    /// Largest requested byte count observed by the profiling layer so far.
    pub process_peak: usize,
}

/// Collects selected closed spans in memory using the heap layer's single interval consumer.
#[derive(Debug, Clone)]
pub struct SpanRecorder {
    names: Arc<[&'static str]>,
    summaries: Arc<Mutex<Vec<SpanSummary>>>,
}

impl SpanRecorder {
    /// Records these names; an empty selection records every span.
    #[must_use]
    pub fn new(names: &[&'static str]) -> Self {
        Self {
            names: names.into(),
            summaries: Arc::default(),
        }
    }

    /// Creates the sole heap-measuring layer for this subscriber.
    #[must_use]
    pub fn layer(&self, allocator: &'static CountingAllocator) -> HeapLayer {
        HeapLayer::recording(allocator, self.clone())
    }

    /// Takes completed summaries, leaving the recorder empty.
    #[must_use]
    pub fn take(&self) -> Vec<SpanSummary> {
        std::mem::take(
            &mut *self
                .summaries
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    pub(crate) fn record(&self, summary: SpanSummary) {
        if self.names.is_empty() || self.names.contains(&summary.name) {
            self.summaries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(summary);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SpanRecorder;
    use crate::CountingAllocator;
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    #[test]
    fn selects_closed_lifetimes_and_drains_summaries() {
        static ALLOCATOR: CountingAllocator = CountingAllocator::new();
        let recorder = SpanRecorder::new(&["selected"]);
        let subscriber = Registry::default().with(recorder.layer(&ALLOCATOR));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("selected");
            tracing::info_span!("ignored").in_scope(|| {});
            assert!(
                recorder.take().is_empty(),
                "an open span must not be reported"
            );
            drop(span);
        });
        let summaries = recorder.take();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "selected");
        assert!(!summaries[0].duration.is_zero());
        assert_eq!(summaries[0].heap_peak, 0);
        assert_eq!(summaries[0].heap_delta, 0);
        assert!(recorder.take().is_empty());
    }
}
