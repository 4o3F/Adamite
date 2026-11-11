use crate::{
    cli::ScanArgs,
    command::info::Info,
    error::CliError,
    render::{json::Json, text::SavedText},
    report::SavedReport,
    save::{PublishMode, SnapshotFile},
    source::Source,
};
use std::io::Write;

pub(crate) struct Scan(pub(crate) ScanArgs);

impl Scan {
    pub(crate) fn run(self, stdout: &mut impl Write) -> Result<(), CliError> {
        let args = self.0;
        let source = Source::open(&args.source.source)?;
        if source.is_snapshot() {
            return Err(CliError::Usage {
                message: "source is already a snapshot",
            });
        }
        let Some(destination) = args.output else {
            let output = source.load()?;
            return Info(args.source).render("scan", &output, stdout);
        };
        let mode = if args.force {
            PublishMode::Replace
        } else {
            PublishMode::NoClobber
        };
        let file = SnapshotFile::new(destination.clone(), mode);
        file.preflight(&source)?;
        let output = source.load()?;
        let bytes = file.save(&output)?;
        let report = SavedReport::new(&args.source.source, &output, &destination, bytes);
        if args.source.json {
            Json(&report).write(stdout)
        } else {
            SavedText {
                report: &report,
                bytes: args.source.bytes,
            }
            .write(stdout)
        }
    }
}
