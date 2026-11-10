use adamite_core::scan::{ScanConfig, ScanContext, ScanOutput, ScanStep, Scanner};
use adamite_fixtures::{
    builders::{AttrBuilder, BootSectorBuilder, FileNameBuilder, NonResidentModel, RecordBuilder},
    runlist_encoder::{RunModel, RunlistEncoder},
};
use std::error::Error;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub fn scan(image: &[u8], context: ScanContext) -> TestResult<ScanOutput> {
    let mut scanner = Scanner::new(ScanConfig {
        context,
        ..ScanConfig::default()
    })?;
    loop {
        match scanner.step()? {
            ScanStep::Need(request) => {
                let start = usize::try_from(request.offset)?;
                let end = start
                    .checked_add(request.len)
                    .ok_or("read range overflow")?;
                scanner.feed(request.id, image.get(start..end).ok_or("truncated image")?)?;
            }
            ScanStep::Progress(_) => {}
            ScanStep::Done(output) => return Ok(output),
            _ => return Err("unsupported scan step".into()),
        }
    }
}

pub fn u32_at(bytes: &[u8], offset: usize) -> TestResult<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("missing u32")?
            .try_into()?,
    ))
}

pub fn u64_at(bytes: &[u8], offset: usize) -> TestResult<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or("missing u64")?
            .try_into()?,
    ))
}

pub fn section(bytes: &[u8], kind: usize) -> TestResult<(usize, usize, usize)> {
    let entry = 64 + (kind - 1) * 32;
    Ok((
        usize::try_from(u64_at(bytes, entry + 8)?)?,
        usize::try_from(u64_at(bytes, entry + 16)?)?,
        usize::try_from(u64_at(bytes, entry + 24)?)?,
    ))
}

pub fn tiny_image() -> Vec<u8> {
    let mut image = vec![0; 128 * 512];
    image[..512].copy_from_slice(
        &BootSectorBuilder {
            cluster_encoding: 1,
            sectors: 128,
            mirror: 64,
            ..BootSectorBuilder::default()
        }
        .build(),
    );
    let name = |text: &str| {
        AttrBuilder::resident(
            0x30,
            FileNameBuilder {
                parent: (7 << 48) | 5,
                name: text.encode_utf16().collect(),
                ..FileNameBuilder::default()
            }
            .build(),
        )
        .build()
    };
    let mft = AttrBuilder::non_resident(
        0x80,
        NonResidentModel {
            highest: 31,
            sizes: [16384; 4],
            mapping_pairs: RunlistEncoder {
                runs: vec![RunModel {
                    lcn: Some(4),
                    len: 32,
                }],
            }
            .encode(0),
            ..NonResidentModel::default()
        },
    )
    .build();
    for record in [
        RecordBuilder {
            record_number: 0,
            attributes: vec![mft, name("$MFT")],
            ..RecordBuilder::default()
        },
        RecordBuilder {
            record_number: 5,
            flags: 3,
            attributes: vec![name(".")],
            ..RecordBuilder::default()
        },
        RecordBuilder {
            record_number: 12,
            attributes: vec![
                name("note.TxT"),
                AttrBuilder::resident(0x80, b"hi".to_vec()).build(),
            ],
            ..RecordBuilder::default()
        },
    ] {
        let start = 4 * 512 + record.record_number as usize * 1024;
        image[start..start + 1024].copy_from_slice(&record.build());
    }
    image
}
