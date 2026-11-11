use crate::{CountingAllocator, HeapLayer};
use snafu::Snafu;
use std::{fmt, fs::File, io, path::Path};
use tracing::subscriber::SetGlobalDefaultError;
use tracing_chrome::{ChromeLayerBuilder, FlushGuard, TraceStyle};
use tracing_subscriber::{Registry, layer::SubscriberExt};

/// A failure to create the trace file or install the process-wide subscriber.
#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum ProfileError {
    /// The destination could not be created or opened for writing.
    #[snafu(display("cannot create Chrome trace"))]
    Create {
        /// The underlying file error.
        source: io::Error,
    },
    /// Another global tracing subscriber has already been installed.
    #[snafu(display("cannot install Chrome profiling subscriber"))]
    Install {
        /// The subscriber installation error.
        source: SetGlobalDefaultError,
    },
}

/// Keeps the Chrome trace writer alive and joins it to finish the JSON file on drop.
///
/// Drop after all instrumented command work and span handles have finished. The global
/// subscriber remains installed for the process lifetime; profiling can be installed once.
#[must_use = "keep the profile guard alive until the instrumented work finishes"]
pub struct ChromeProfile {
    _flush: FlushGuard,
}

impl fmt::Debug for ChromeProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChromeProfile")
            .finish_non_exhaustive()
    }
}

impl ChromeProfile {
    /// Installs Chrome tracing with heap events from the executable's allocator.
    ///
    /// Spans use asynchronous lifetimes so stages can span several driver calls without
    /// retaining an entered guard. Heap events carry the span name and ID in `args`.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError`] if the file cannot be created or a global subscriber exists.
    pub fn install(
        path: impl AsRef<Path>,
        allocator: &'static CountingAllocator,
    ) -> Result<Self, ProfileError> {
        let file = File::create(path).map_err(|source| ProfileError::Create { source })?;
        let (chrome, flush) = ChromeLayerBuilder::new()
            .writer(file)
            .include_args(true)
            .trace_style(TraceStyle::Async)
            .build();
        // Inner layers close first: emit the heap event before Chrome ends the span.
        let subscriber = Registry::default()
            .with(HeapLayer::new(allocator))
            .with(chrome);
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|source| ProfileError::Install { source })?;
        Ok(Self { _flush: flush })
    }
}

#[cfg(test)]
mod tests {
    use super::ChromeProfile;
    use crate::CountingAllocator;
    use std::{error::Error, fs};

    #[test]
    fn guard_finishes_json_with_span_and_heap_arguments() -> Result<(), Box<dyn Error>> {
        static ALLOCATOR: CountingAllocator = CountingAllocator::new();
        let path =
            std::env::temp_dir().join(format!("adamite-profile-{}.json", std::process::id()));
        let guard = ChromeProfile::install(&path, &ALLOCATOR)?;
        tracing::info_span!("profile_test").in_scope(|| {});
        drop(guard);
        let trace: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        fs::remove_file(path)?;
        let events = trace.as_array().ok_or("trace is not an array")?;
        let start = events
            .iter()
            .position(|event| event["name"] == "profile_test" && event["ph"] == "b")
            .ok_or("missing span start")?;
        let end = events
            .iter()
            .position(|event| event["name"] == "profile_test" && event["ph"] == "e")
            .ok_or("missing span end")?;
        let heap = events
            .iter()
            .position(|event| {
                event["name"] == "heap_usage" && event["args"]["span_name"] == "profile_test"
            })
            .ok_or("missing heap event")?;
        assert!(
            start < heap && heap < end,
            "heap event must be inside the span lifetime"
        );
        for field in ["heap_current", "heap_peak", "heap_delta", "process_peak"] {
            assert_eq!(events[heap]["args"][field], "0");
        }
        assert!(
            events[heap]["args"]["span_id"]
                .as_str()
                .ok_or("missing span ID")?
                .parse::<u64>()?
                > 0
        );
        Ok(())
    }
}
