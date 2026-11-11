use adamite_core::scan::ScanPhase;
use adamite_io::driver::DriverProgress;
use indicatif::{HumanBytes, ProgressBar, ProgressState, ProgressStyle, style::TemplateError};
use std::{
    fmt,
    io::{self, IsTerminal, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub(crate) struct ScanProgressView {
    bar: ProgressBar,
    stream_style: ProgressStyle,
    spinner_style: ProgressStyle,
    phase: Option<(ScanPhase, bool)>,
    bytes_read: Arc<AtomicU64>,
}

impl ScanProgressView {
    const STREAM_TEMPLATE: &str =
        "{msg} [{wide_bar}] {pos}/{len} {percent}% {read_bytes} {read_rate}/s ETA {eta}";
    const SPINNER_TEMPLATE: &str = "{spinner} {msg}";

    pub(crate) fn new(path: &Path) -> Option<Self> {
        if !io::stderr().is_terminal() {
            let _ = writeln!(io::stderr().lock(), "scanning {}…", path.display());
            return None;
        }
        let bytes_read = Arc::new(AtomicU64::new(0));
        let stream_style = Self::stream_style(Arc::clone(&bytes_read))
            .unwrap_or_else(|_| ProgressStyle::default_bar());
        let spinner_style =
            Self::spinner_style().unwrap_or_else(|_| ProgressStyle::default_spinner());
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style.clone());
        bar.set_message(Self::phase_label(ScanPhase::Boot));
        bar.enable_steady_tick(Duration::from_millis(100));
        Some(Self {
            bar,
            stream_style,
            spinner_style,
            phase: None,
            bytes_read,
        })
    }

    fn stream_style(bytes_read: Arc<AtomicU64>) -> Result<ProgressStyle, TemplateError> {
        let rate_bytes = Arc::clone(&bytes_read);
        Ok(ProgressStyle::with_template(Self::STREAM_TEMPLATE)?
            .with_key(
                "read_bytes",
                move |_: &ProgressState, writer: &mut dyn fmt::Write| {
                    let _ = write!(writer, "{}", HumanBytes(bytes_read.load(Ordering::Relaxed)));
                },
            )
            .with_key(
                "read_rate",
                move |state: &ProgressState, writer: &mut dyn fmt::Write| {
                    let rate = u128::from(rate_bytes.load(Ordering::Relaxed))
                        .saturating_mul(1_000_000_000)
                        .checked_div(state.elapsed().as_nanos())
                        .unwrap_or(0);
                    let _ = write!(
                        writer,
                        "{}",
                        HumanBytes(u64::try_from(rate).unwrap_or(u64::MAX))
                    );
                },
            ))
    }

    fn spinner_style() -> Result<ProgressStyle, TemplateError> {
        ProgressStyle::with_template(Self::SPINNER_TEMPLATE)
    }

    fn phase_label(phase: ScanPhase) -> &'static str {
        match phase {
            ScanPhase::Boot => "booting",
            ScanPhase::Record0 | ScanPhase::Discover | ScanPhase::Stream => "reading MFT",
            ScanPhase::Merge => "merging",
            ScanPhase::Link => "linking",
            ScanPhase::Snapshot => "building",
            ScanPhase::Aggregate => "aggregating",
            ScanPhase::Finalize | ScanPhase::Done => "finalizing",
            ScanPhase::Failed => "failed",
            _ => "scanning",
        }
    }

    pub(crate) fn update(&mut self, progress: DriverProgress) {
        self.bytes_read
            .store(progress.bytes_read, Ordering::Relaxed);
        let streaming =
            progress.scan.phase == ScanPhase::Stream && progress.scan.records_planned.is_some();
        let phase = (progress.scan.phase, streaming);
        if self.phase != Some(phase) {
            if streaming && let Some(total) = progress.scan.records_planned {
                self.bar.set_length(total);
            }
            self.bar.set_style(if streaming {
                self.stream_style.clone()
            } else {
                self.spinner_style.clone()
            });
            self.bar.set_message(Self::phase_label(progress.scan.phase));
            self.phase = Some(phase);
        }
        if streaming {
            self.bar.set_position(progress.scan.records_processed);
        }
    }
}

impl Drop for ScanProgressView {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::ScanProgressView;
    use adamite_core::scan::ScanPhase;
    use std::{
        error::Error,
        sync::{Arc, atomic::AtomicU64},
    };

    #[test]
    fn phase_labels_cover_the_scan_pipeline() {
        for (phase, label) in [
            (ScanPhase::Boot, "booting"),
            (ScanPhase::Record0, "reading MFT"),
            (ScanPhase::Discover, "reading MFT"),
            (ScanPhase::Stream, "reading MFT"),
            (ScanPhase::Merge, "merging"),
            (ScanPhase::Link, "linking"),
            (ScanPhase::Snapshot, "building"),
            (ScanPhase::Aggregate, "aggregating"),
            (ScanPhase::Finalize, "finalizing"),
            (ScanPhase::Done, "finalizing"),
            (ScanPhase::Failed, "failed"),
        ] {
            assert_eq!(ScanProgressView::phase_label(phase), label);
        }
    }

    #[test]
    fn templates_accept_record_and_independent_read_counters() -> Result<(), Box<dyn Error>> {
        ScanProgressView::stream_style(Arc::new(AtomicU64::new(0)))?;
        ScanProgressView::spinner_style()?;
        for key in [
            "{pos}",
            "{len}",
            "{percent}",
            "{read_bytes}",
            "{read_rate}",
            "{eta}",
        ] {
            assert!(
                ScanProgressView::STREAM_TEMPLATE.contains(key),
                "missing {key}"
            );
        }
        Ok(())
    }
}
