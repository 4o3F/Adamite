//! Checks that deterministic seeds reach Done and preflight rejects oversized inputs.

#[path = "../fuzz_targets/scan/driver.rs"]
mod driver;
#[path = "../fuzz_targets/scan/input.rs"]
mod input;
#[path = "../fuzz_targets/persist/driver.rs"]
mod persist;
#[path = "../fuzz_targets/scan/volume.rs"]
mod volume;

use adamite_core::scan::{ScanStep, Scanner};
use std::{error::Error, fs, path::Path};

#[test]
fn persist_corpus_round_trips_or_rejects() -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/persist");
    let mut accepted = 0;
    let mut seeds = 0;
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let bytes = fs::read(entry?.path())?;
        assert!(bytes.len() <= 65_536, "persist seed exceeds byte budget");
        accepted += usize::from(persist::exercise(&bytes));
        seeds += 1;
        total += bytes.len();
    }
    assert!(seeds > 0, "persist corpus must not be empty");
    assert!(accepted > 0, "persist corpus must contain a valid encoding");
    assert!(
        total <= 1_048_576,
        "persist corpus exceeds total byte budget"
    );
    Ok(())
}

fn assert_seed(bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let input = input::Input::parse(bytes)?;
    let image = input.image();
    let records = input::preflight(&image).ok_or("seed rejected by preflight")?;
    driver::scan(&image, input.config(), &input.events, records);
    // The fuzz driver permits errors for mutated inputs; valid seeds must reach Done.
    let mut scanner = Scanner::new(input.config())?;
    for _ in 0..4096 + 64 * records + 16 * image.len().div_ceil(512) {
        match scanner.step()? {
            ScanStep::Need(request) => {
                let start = usize::try_from(request.offset)?;
                scanner.feed(
                    request.id,
                    image
                        .get(start..start + request.len)
                        .ok_or("seed read outside image")?,
                )?;
            }
            ScanStep::Progress(_) => {}
            ScanStep::Done(_) => return Ok(()),
            _ => return Err("unknown step".into()),
        }
    }
    Err("seed exceeded step bound".into())
}

#[test]
fn deterministic_corpus_completes() -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/scan");
    let mut seeds = 0;
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        let bytes = fs::read(&path)?;
        assert_seed(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        total += bytes.len();
        seeds += 1;
    }
    assert_eq!(seeds, 131);
    assert!(total <= 65_536);
    Ok(())
}

#[test]
fn advertised_mft_record_bound_is_checked() {
    let mut image = volume::build(volume::Layout::Contiguous, 0);
    // Record 0 starts at LCN 4; its first attribute is at byte 56.
    let data_size = 4 * 512 + 56 + 48;
    image[data_size..data_size + 8].copy_from_slice(&(4097u64 * 1024).to_le_bytes());
    assert_eq!(input::preflight(&image), None);
}

#[test]
fn list_bound_covers_extension_descriptors_and_split_headers() {
    for storage in [0, 1] {
        let mut image = volume::build(volume::Layout::Fragmented, 0);
        // Unused image bytes stand for a descriptor in any reachable extension.
        let start = 120 * 512;
        image[start..start + 4].copy_from_slice(&0x20u32.to_le_bytes());
        image[start + 4..start + 8].copy_from_slice(&64u32.to_le_bytes());
        image[start + 8] = storage;
        if storage == 0 {
            image[start + 16..start + 20].copy_from_slice(&262_145u32.to_le_bytes());
        } else {
            image[start + 48..start + 56].copy_from_slice(&262_145u64.to_le_bytes());
        }
        assert_eq!(input::preflight(&image), None);
    }
    let mut image = volume::build(volume::Layout::Fragmented, 0);
    let start = 120 * 512 + 456;
    image[start..start + 4].copy_from_slice(&0x20u32.to_le_bytes());
    image[start + 4..start + 8].copy_from_slice(&64u32.to_le_bytes());
    assert_eq!(input::preflight(&image), None);
}

#[test]
fn raw_images_and_mutation_counts_are_bounded() -> Result<(), Box<dyn Error>> {
    let mut bytes = vec![2, 15, 0, 255, 255, 255, 255, 255, 255, 255, 255, 16];
    for _ in 0..16 {
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.push(1);
    }
    bytes.resize(bytes.len() + input::MAX_IMAGE + 512, 0);
    let input = input::Input::parse(&bytes)?;
    assert_eq!(input.image().len(), input::MAX_IMAGE);
    assert_eq!(input.image()[0], 0, "sixteen XOR mutations cancel");
    assert_eq!(
        (input.config().chunk_size, input.config().max_outstanding),
        (4096, 4)
    );
    Ok(())
}
