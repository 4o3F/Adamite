#[path = "../fuzz_targets/scan/volume.rs"]
mod volume;

use adamite_core::scan::{ScanConfig, ScanContext, ScanOutput, ScanStep, Scanner};
use adamite_fixtures::{
    builders::{AttrBuilder, BootSectorBuilder, FileNameBuilder, NonResidentModel, RecordBuilder},
    runlist_encoder::{RunModel, RunlistEncoder},
};
use std::{env, error::Error, fs, path::Path};

const SEED_BUDGET: usize = 65_536;
const TOTAL_BUDGET: usize = 1_048_576;

fn tiny_image() -> Vec<u8> {
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

fn scan(image: &[u8], context: ScanContext) -> Result<ScanOutput, Box<dyn Error>> {
    let mut scanner = Scanner::new(ScanConfig {
        chunk_size: 4096,
        context,
        ..ScanConfig::default()
    })?;
    for _ in 0..4096 + 64 * 24 + 16 * image.len().div_ceil(512) {
        match scanner.step()? {
            ScanStep::Need(request) => {
                let start = usize::try_from(request.offset)?;
                let end = start.checked_add(request.len).ok_or("read overflow")?;
                scanner.feed(request.id, image.get(start..end).ok_or("truncated image")?)?;
            }
            ScanStep::Progress(_) => {}
            ScanStep::Done(output) => return Ok(output),
            _ => return Err("unsupported scan step".into()),
        }
    }
    Err("seed scan exceeded step budget".into())
}

fn repair_crc(bytes: &mut [u8]) {
    bytes[32..36].fill(0);
    let crc = crc32fast::hash(bytes);
    bytes[32..36].copy_from_slice(&crc.to_le_bytes());
}

fn write_variants(
    out: &Path,
    name: &str,
    output: ScanOutput,
    total: &mut usize,
) -> Result<(), Box<dyn Error>> {
    let valid = output.to_bytes()?;
    if valid.len() > SEED_BUDGET {
        eprintln!(
            "skipping {name}: {} bytes exceeds {SEED_BUDGET}",
            valid.len()
        );
        return Ok(());
    }
    assert_eq!(ScanOutput::from_bytes(&valid)?, output);
    let truncated = valid[..valid.len() / 2].to_vec();
    let mut corrupt = valid.clone();
    corrupt[32] ^= 1;
    let mut mutation = valid.clone();
    let nodes = usize::try_from(u64::from_le_bytes(valid[136..144].try_into()?))?;
    mutation[nodes] ^= 1;
    repair_crc(&mut mutation);
    for (variant, bytes) in [
        ("valid", valid),
        ("truncated", truncated),
        ("crc", corrupt),
        ("nodes", mutation),
    ] {
        assert!(bytes.len() <= SEED_BUDGET, "seed exceeds per-file budget");
        *total += bytes.len();
        assert!(
            *total <= TOTAL_BUDGET,
            "persist corpus exceeds total budget"
        );
        fs::write(out.join(format!("{name}-{variant}")), bytes)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let out = args.next().ok_or("usage: persist_seeds <out-dir>")?;
    if args.next().is_some() {
        return Err("usage: persist_seeds <out-dir>".into());
    }
    let out = Path::new(&out);
    fs::create_dir_all(out)?;
    let mut total = 0;
    write_variants(
        out,
        "tiny",
        scan(&tiny_image(), ScanContext::default())?,
        &mut total,
    )?;
    for (name, layout, options) in [
        ("nested", volume::Layout::Contiguous, 1),
        ("hardlinks", volume::Layout::Contiguous, 7),
        ("sparse-ads", volume::Layout::Fragmented, 15),
    ] {
        let context = ScanContext {
            label: Some(name.to_owned()),
            scanned_at_filetime: Some(123),
            free_bytes: Some(4096),
        };
        write_variants(
            out,
            name,
            scan(&volume::build(layout, options), context)?,
            &mut total,
        )?;
    }
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/scan");
    let mut paths = fs::read_dir(corpus)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut reduced = 0;
    for path in paths {
        let bytes = fs::read(path)?;
        if bytes.len() < 12 || bytes[0] != 2 || bytes[11] != 0 {
            continue;
        }
        write_variants(
            out,
            &format!("fixture-{reduced}"),
            scan(&bytes[12..], ScanContext::default())?,
            &mut total,
        )?;
        reduced += 1;
    }
    assert_eq!(reduced, 3, "expected three reduced fixture images");
    println!("persist seeds: {total}/{TOTAL_BUDGET} bytes");
    Ok(())
}
