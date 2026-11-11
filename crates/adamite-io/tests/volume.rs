//! Windows drive discovery on the CI runner's NTFS system volume.

#![cfg(windows)]

use adamite_io::volume::Volume;
use std::error::Error;

#[test]
fn lists_system_volume_in_letter_order() -> Result<(), Box<dyn Error>> {
    let volumes = Volume::list()?;
    let system = volumes
        .iter()
        .find(|volume| volume.letter() == 'C')
        .ok_or("C volume missing")?;
    assert!(system.capacity() > 0);
    assert!(volumes.windows(2).all(|pair| match pair {
        [left, right] => left.letter() < right.letter(),
        _ => false,
    }));
    Ok(())
}
