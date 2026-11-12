use crate::{
    cli::LimitArgs, elevated_scan::ElevatedScan, error::CliError, save::PublishMode,
    scan_progress_view::ScanProgressView, status_file::StatusFile,
};
use adamite_access::Source;
use adamite_core::scan::ScanOutput;
use adamite_io::driver::DriverProgress;
use std::path::Path;

pub(crate) trait SourceExt {
    fn elevation(
        &self,
        limits: &LimitArgs,
        destination: Option<(&Path, PublishMode)>,
    ) -> Result<Option<ElevatedScan>, CliError>;

    fn load(self, limits: &LimitArgs) -> Result<ScanOutput, CliError>;

    fn load_cli(
        self,
        limits: &LimitArgs,
        status: Option<&mut StatusFile>,
    ) -> Result<ScanOutput, CliError>;

    fn load_observed_cli(
        self,
        limits: &LimitArgs,
        observer: impl FnMut(DriverProgress),
    ) -> Result<ScanOutput, CliError>;
}

impl SourceExt for Source {
    fn elevation(
        &self,
        limits: &LimitArgs,
        destination: Option<(&Path, PublishMode)>,
    ) -> Result<Option<ElevatedScan>, CliError> {
        if matches!(self, Source::Drive { .. }) && ElevatedScan::required(limits)? {
            ElevatedScan::new(destination).map(Some)
        } else {
            Ok(None)
        }
    }

    fn load(self, limits: &LimitArgs) -> Result<ScanOutput, CliError> {
        if let Some(elevated) = self.elevation(limits, None)?
            && let Source::Drive { path, .. } = &self
        {
            return elevated.run(path, limits, || ScanProgressView::observer(path, None));
        }
        self.load_cli(limits, None)
    }

    fn load_cli(
        self,
        limits: &LimitArgs,
        status: Option<&mut StatusFile>,
    ) -> Result<ScanOutput, CliError> {
        if self.is_snapshot() || (!cfg!(windows) && matches!(self, Source::Drive { .. })) {
            return self.load_observed_cli(limits, |_| {});
        }
        let observer = ScanProgressView::observer(self.path(), status);
        self.load_observed_cli(limits, observer)
    }

    fn load_observed_cli(
        self,
        limits: &LimitArgs,
        observer: impl FnMut(DriverProgress),
    ) -> Result<ScanOutput, CliError> {
        self.load_observed(&limits.limits(), observer)
            .map_err(|error| {
                let mut error = CliError::from(error);
                if let CliError::Volume { no_elevate, .. } = &mut error {
                    *no_elevate = limits.no_elevate;
                }
                error
            })
    }
}
