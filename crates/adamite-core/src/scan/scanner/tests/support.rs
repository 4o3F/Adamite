//! Shared synthetic builders and bounded fixture drivers.

use crate::error::ScanError;
use crate::scan::{ReadRequest, ScanPhase, ScanStep, Scanner};
use adamite_fixtures::VolumeImage;
use adamite_fixtures::builders::{
    AttrBuilder, BootSectorBuilder, FileNameBuilder, NonResidentModel, RecordBuilder,
};
use adamite_fixtures::runlist_encoder::{RunModel, RunlistEncoder};
use std::error::Error;
use std::io::Cursor;

pub(super) type TestResult = Result<(), Box<dyn Error>>;
pub(super) type ObservedDriveOutcome = (Vec<ReadRequest>, Result<(), ScanError>, usize);
pub(super) type DriveOutcome = (Vec<ReadRequest>, Result<(), ScanError>);
pub(super) type DriverOutcome = (Scanner, Vec<ReadRequest>, Result<(), ScanError>);

pub(super) fn boot() -> BootSectorBuilder {
    BootSectorBuilder {
        sectors: 256,
        ..BootSectorBuilder::default()
    }
}

pub(super) fn data() -> AttrBuilder {
    AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 3,
            sizes: [16_384, 16_384, 16_384, 0],
            mapping_pairs: vec![0x11, 4, 4, 0],
            ..NonResidentModel::default()
        },
    )
}

pub(super) fn record() -> RecordBuilder {
    RecordBuilder {
        record_number: 0,
        attributes: vec![data().build()],
        ..RecordBuilder::default()
    }
}

pub(super) fn image(boot: &BootSectorBuilder, record: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let layout = VolumeImage::new(boot.clone(), 131_072)?;
    let mut writer = Cursor::new(Vec::new());
    layout.write(&mut writer)?;
    layout.write_record(&mut writer, 0, record)?;
    let image = writer.into_inner();
    Ok(image)
}

pub(super) fn drive_to(
    scanner: &mut Scanner,
    image: &[u8],
    stop: ScanPhase,
) -> Result<DriveOutcome, Box<dyn Error>> {
    let (requests, outcome, _) = drive_to_observed(scanner, image, stop)?;
    Ok((requests, outcome))
}

pub(super) fn drive_to_observed(
    scanner: &mut Scanner,
    image: &[u8],
    stop: ScanPhase,
) -> Result<ObservedDriveOutcome, Box<dyn Error>> {
    let mut peak_transport_bytes = 0usize;
    let mut requests = Vec::new();
    let mut pending = Vec::new();
    let calls = image
        .len()
        .checked_div(512)
        .and_then(|n| n.checked_mul(16))
        .and_then(|n| n.checked_add(4096))
        .ok_or("driver step bound")?;
    let mut previous_records = scanner
        .ingest
        .as_ref()
        .map_or(0, |state| state.stats.records_total);
    for _ in 0..calls {
        let result = scanner.step();
        peak_transport_bytes = peak_transport_bytes.max(assert_transport_bound(scanner)?);
        match result {
            Ok(ScanStep::Need(request)) => {
                assert!(request.len <= scanner.config.chunk_size);
                requests.push(request);
                pending.push(request);
                assert!(pending.len() <= scanner.config.max_outstanding);
            }
            Ok(ScanStep::Progress(progress)) => {
                let processed = progress
                    .records_processed
                    .checked_sub(previous_records)
                    .ok_or("progress moved backwards")?;
                assert!(processed <= 256, "CPU step processed too many records");
                previous_records = progress.records_processed;
                if progress.phase == stop {
                    return Ok((requests, Ok(()), peak_transport_bytes));
                }
                if matches!(
                    progress.phase,
                    ScanPhase::Boot | ScanPhase::Record0 | ScanPhase::Discover
                ) {
                    assert_eq!(
                        progress.records_processed, 0,
                        "bootstrap must not count records"
                    );
                }
                if progress.waiting_for_input {
                    assert!(
                        !pending.is_empty(),
                        "waiting without an outstanding request"
                    );
                    for request in pending.drain(..).rev() {
                        let start = usize::try_from(request.offset)?;
                        let end = start
                            .checked_add(request.len)
                            .ok_or("response end overflow")?;
                        scanner.feed(
                            request.id,
                            image.get(start..end).ok_or("response image slice")?,
                        )?;
                        peak_transport_bytes =
                            peak_transport_bytes.max(assert_transport_bound(scanner)?);
                    }
                }
            }
            Err(error) => return Ok((requests, Err(error), peak_transport_bytes)),
            Ok(ScanStep::Done(_)) => return Err("bootstrap unexpectedly completed the scan".into()),
        }
    }
    Err("bootstrap did not reach a terminal boundary".into())
}

pub(super) fn segment(id: u16, lowest: i64, highest: i64, lcn: u64, len: u64) -> AttrBuilder {
    let mut attribute = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            lowest,
            highest,
            sizes: [6144, 6144, 6144, 0],
            mapping_pairs: RunlistEncoder {
                runs: vec![RunModel {
                    lcn: Some(lcn),
                    len,
                }],
            }
            .encode(0),
            ..NonResidentModel::default()
        },
    );
    attribute.id = id;
    attribute
}

pub(super) fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    let end = offset
        .checked_add(bytes.len())
        .ok_or("synthetic extent end")?;
    image
        .get_mut(offset..end)
        .ok_or("synthetic extent slice")?
        .copy_from_slice(bytes);
    Ok(())
}

pub(super) fn assert_transport_bound(scanner: &Scanner) -> Result<usize, Box<dyn Error>> {
    if let Some(state) = &scanner.ingest {
        let record_size = usize::try_from(
            scanner
                .bootstrap
                .as_ref()
                .ok_or("stream mapping")?
                .mapping
                .geometry
                .file_record_size(),
        )?;
        let bound = scanner
            .config
            .max_outstanding
            .checked_mul(scanner.config.chunk_size)
            .and_then(|n| n.checked_add(record_size))
            .ok_or("transport byte bound")?;
        let retained = Scanner::stream_transport_bytes(state, &scanner.requests)?;
        assert!(
            retained <= bound,
            "retained raw MFT exceeds cap * chunk + record"
        );
        return Ok(retained);
    }
    Ok(0)
}

pub(super) fn named_record(number: u32, name: Vec<u16>) -> RecordBuilder {
    let filename = FileNameBuilder {
        name,
        parent: (7u64 << 48) | 5,
        ..FileNameBuilder::default()
    };
    RecordBuilder {
        record_number: number,
        attributes: vec![AttrBuilder::resident(0x30, filename.build()).build()],
        ..RecordBuilder::default()
    }
}

pub(super) fn ingest_volume(
    count: u64,
    records: Vec<(usize, Vec<u8>)>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let boot = BootSectorBuilder {
        sectors: 2048,
        cluster_encoding: 1,
        mft: 8,
        mirror: 1900,
        ..boot()
    };
    let size = count.checked_mul(1024).ok_or("synthetic MFT length")?;
    let layout = VolumeImage::new(boot.clone(), 1_048_576)?;
    let clusters = size.checked_div(512).ok_or("synthetic MFT clusters")?;
    let first = layout.non_resident(0x80, 8, clusters, size)?;
    let base = RecordBuilder {
        attributes: vec![first.build()],
        ..record()
    };
    let mut writer = Cursor::new(Vec::new());
    layout.write(&mut writer)?;
    layout.write_record(&mut writer, 0, &base.build())?;
    let root = RecordBuilder {
        flags: 3,
        ..named_record(5, vec![46])
    };
    layout.write_record(&mut writer, 5, &root.build())?;
    for (ordinal, bytes) in records {
        layout.write_record(&mut writer, u64::try_from(ordinal)?, &bytes)?;
    }
    Ok(writer.into_inner())
}
