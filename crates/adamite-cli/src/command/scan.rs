use crate::{
    cli::ScanArgs, command::info::Info, error::CliError, render::Renderer,
    scan_progress_view::ScanProgressView, source::SourceExt,
};
use adamite_access::report::SavedReport;
use adamite_access::{PublishMode, SnapshotFile, Source, StatusFile};
use adamite_io::status::ScanStatus;
use std::io::Write;

pub(crate) struct Scan(pub(crate) ScanArgs);

impl Scan {
    pub(crate) fn run(self, stdout: &mut impl Write) -> Result<(), CliError> {
        let mut status = self
            .0
            .status_file
            .as_deref()
            .map(StatusFile::create)
            .transpose()?;
        let result = self.execute(&mut status, stdout);
        if let Some(status) = &mut status {
            match &result {
                Ok(Some(outcome)) => status.finish(outcome)?,
                Ok(None) => {}
                Err(error) => {
                    let _ = status.finish(&ScanStatus::Failed {
                        message: error.message(),
                        exit_code: i32::from(error.exit_code()),
                    });
                }
            }
        }
        result.map(|_| ())
    }

    fn execute(
        self,
        status: &mut Option<StatusFile>,
        stdout: &mut impl Write,
    ) -> Result<Option<ScanStatus>, CliError> {
        let args = self.0;
        let source = Source::open(&args.source.source)?;
        if source.is_snapshot() {
            return Err(CliError::Usage {
                message: "source is already a snapshot",
            });
        }
        let Some(destination) = args.output else {
            let output = source.load(&args.source.limits)?;
            Info(args.source).render("scan", &output, stdout)?;
            return Ok(None);
        };
        let mode = if args.force {
            PublishMode::Replace
        } else {
            PublishMode::NoClobber
        };
        let file = SnapshotFile::new(destination.clone(), mode);
        file.preflight(&source)?;
        let elevated = source.elevation(&args.source.limits, Some((&destination, mode)))?;
        let saved_by_child = elevated.is_some();
        let output = if let Some(elevated) = elevated {
            elevated.run(&args.source.source, &(&args.source.limits).into(), || {
                ScanProgressView::observer(&args.source.source, status.as_mut())
            })?
        } else {
            source.load_cli(&args.source.limits, status.as_mut())?
        };
        if let Some(status) = status {
            status.check()?;
        }
        let bytes = if saved_by_child {
            file.length()?
        } else {
            file.save(&output)?
        };
        let report = SavedReport::new(&args.source.source, &output, &destination, bytes);
        if let Err(error) = Renderer::from(&args.source).write(&report, stdout)
            && !error.is_broken_pipe()
        {
            return Err(error);
        }
        Ok(Some(ScanStatus::Done {
            saved: destination.to_string_lossy().into_owned(),
            bytes: u64::try_from(bytes)
                .map_err(|_| CliError::SnapshotLength { path: destination })?,
        }))
    }
}
