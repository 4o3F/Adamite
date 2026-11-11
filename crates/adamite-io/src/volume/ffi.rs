#![allow(unsafe_code)]
#![deny(
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block
)]

use super::{Volume, VolumeListError};
use std::io;
use windows::{
    Win32::{
        Foundation::MAX_PATH,
        Storage::FileSystem::{
            GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
        },
        System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE},
    },
    core::PCWSTR,
};

fn query_error(source: io::Error) -> VolumeListError {
    VolumeListError::Query { source }
}

fn drive_roots() -> Result<Vec<u16>, VolumeListError> {
    // SAFETY: A null output requests the number of UTF-16 units needed, without writing memory.
    let mut required = unsafe { GetLogicalDriveStringsW(None) };
    loop {
        if required == 0 {
            return Err(query_error(io::Error::last_os_error()));
        }
        let count =
            usize::try_from(required).map_err(|error| query_error(io::Error::other(error)))?;
        let mut roots = Vec::new();
        roots.try_reserve_exact(count)?;
        roots.resize(count, 0);
        // SAFETY: roots is initialized and writable for its full length; the wrapper supplies that length to Windows.
        let written = unsafe { GetLogicalDriveStringsW(Some(&mut roots)) };
        if written == 0 {
            return Err(query_error(io::Error::last_os_error()));
        }
        if written < required {
            return Ok(roots);
        }
        required = written
            .checked_add(1)
            .ok_or_else(|| query_error(io::Error::other("logical-drive list length overflow")))?;
    }
}

fn terminated(units: &[u16]) -> Option<&[u16]> {
    units.get(..units.iter().position(|unit| *unit == 0)?)
}

fn volume(root: &[u16]) -> Option<Volume> {
    let [letter, colon, slash] = root else {
        return None;
    };
    let letter = char::from_u32(u32::from(*letter))?;
    if !letter.is_ascii_alphabetic() || *colon != u16::from(b':') || *slash != u16::from(b'\\') {
        return None;
    }
    let root = [u16::try_from(u32::from(letter)).ok()?, *colon, *slash, 0];
    // SAFETY: root is an initialized, NUL-terminated drive-root string that lives through the call.
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    if !matches!(drive_type, DRIVE_FIXED | DRIVE_REMOVABLE) {
        return None;
    }
    let mut label = [0u16; MAX_PATH as usize + 1];
    let mut filesystem = [0u16; MAX_PATH as usize + 1];
    // SAFETY: root stays NUL-terminated and live; both output arrays are initialized and writable. The wrapper supplies their sizes, and unused outputs are null.
    unsafe {
        GetVolumeInformationW(
            PCWSTR(root.as_ptr()),
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut filesystem),
        )
    }
    .ok()?;
    let filesystem = String::from_utf16(terminated(&filesystem)?).ok()?;
    if filesystem != "NTFS" {
        return None;
    }
    let label = String::from_utf16(terminated(&label)?).ok()?;
    let mut capacity = 0;
    // SAFETY: root remains valid throughout the call, and capacity is an aligned writable u64 for the total-capacity output. Other outputs are null.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), None, Some(&mut capacity), None) }.ok()?;
    Some(Volume {
        letter: letter.to_ascii_uppercase(),
        label,
        capacity,
    })
}

pub(super) fn list() -> Result<Vec<Volume>, VolumeListError> {
    let roots = drive_roots()?;
    let mut volumes = Vec::new();
    for root in roots
        .split(|unit| *unit == 0)
        .take_while(|root| !root.is_empty())
    {
        if let Some(volume) = volume(root) {
            volumes.try_reserve(1)?;
            volumes.push(volume);
        }
    }
    volumes.sort_unstable_by_key(Volume::letter);
    Ok(volumes)
}
