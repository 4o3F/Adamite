#![allow(unsafe_code)]
#![deny(
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block
)]

use std::{fs::File, io, mem::size_of, os::windows::io::AsRawHandle, ptr};
use windows::{
    Win32::{
        Foundation::{HANDLE, MAX_PATH},
        Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW},
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                FSCTL_ALLOW_EXTENDED_DASD_IO, GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO,
                IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
                STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
                StorageAccessAlignmentProperty,
            },
        },
    },
    core::{HSTRING, PCWSTR},
};

#[derive(Debug)]
pub(super) struct VolumeHandle<'a>(&'a File);

impl<'a> VolumeHandle<'a> {
    pub(super) fn new(file: &'a File) -> Self {
        Self(file)
    }

    pub(super) fn allow_extended_dasd(&self) -> io::Result<()> {
        let handle = HANDLE(self.0.as_raw_handle());
        let mut returned = 0;
        // SAFETY: The borrowed File keeps the synchronous handle open. This control code has no
        // input or output buffer; returned is writable for the call and no OVERLAPPED is used.
        unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ALLOW_EXTENDED_DASD_IO,
                None,
                0,
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .map_err(io::Error::from)
    }

    pub(super) fn length(&self) -> io::Result<u64> {
        let handle = HANDLE(self.0.as_raw_handle());
        let mut info = GET_LENGTH_INFORMATION::default();
        let size = u32::try_from(size_of::<GET_LENGTH_INFORMATION>()).map_err(io::Error::other)?;
        let mut returned = 0;
        // SAFETY: The borrowed File keeps the synchronous handle open. info is initialized,
        // aligned and writable for exactly size bytes. returned is writable until the call
        // completes. This query takes no input or OVERLAPPED.
        unsafe {
            DeviceIoControl(
                handle,
                IOCTL_DISK_GET_LENGTH_INFO,
                None,
                0,
                Some(ptr::from_mut(&mut info).cast()),
                size,
                Some(&mut returned),
                None,
            )
        }
        .map_err(io::Error::from)?;
        if returned < size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete volume length response",
            ));
        }
        u64::try_from(info.Length)
            .ok()
            .filter(|len| *len > 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "volume length must be positive")
            })
    }

    pub(super) fn access_alignment(&self) -> io::Result<(u32, u32)> {
        let handle = HANDLE(self.0.as_raw_handle());
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageAccessAlignmentProperty,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        let query_size =
            u32::try_from(size_of::<STORAGE_PROPERTY_QUERY>()).map_err(io::Error::other)?;
        let mut descriptor = STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR::default();
        let descriptor_size = u32::try_from(size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>())
            .map_err(io::Error::other)?;
        let mut returned = 0;
        // SAFETY: The borrowed File keeps the synchronous handle open. query is a live
        // STORAGE_PROPERTY_QUERY of query_size bytes. descriptor is initialized, aligned and
        // writable for descriptor_size bytes. Both and the writable returned count live until
        // the call completes, without an OVERLAPPED.
        unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(ptr::from_ref(&query).cast()),
                query_size,
                Some(ptr::from_mut(&mut descriptor).cast()),
                descriptor_size,
                Some(&mut returned),
                None,
            )
        }
        .map_err(io::Error::from)?;
        if returned < descriptor_size
            || descriptor.Version < descriptor_size
            || descriptor.Size < descriptor_size
            || descriptor.Size > returned
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid storage access alignment descriptor",
            ));
        }
        Ok((
            descriptor.BytesPerLogicalSector,
            descriptor.BytesPerPhysicalSector,
        ))
    }
}

#[derive(Debug)]
pub(super) struct VolumeRoot(HSTRING);

impl VolumeRoot {
    pub(super) fn new(letter: char) -> Self {
        Self(HSTRING::from(format!("{letter}:\\")))
    }

    pub(super) fn label(&self) -> io::Result<String> {
        let mut label = [0u16; MAX_PATH as usize + 1];
        // SAFETY: The HSTRING owns a NUL-terminated root path for the whole call. label is a
        // writable MAX_PATH+1 buffer whose length the wrapper passes in UTF-16 units. All unused
        // output pointers are null.
        unsafe {
            GetVolumeInformationW(
                PCWSTR(self.0.as_ptr()),
                Some(&mut label),
                None,
                None,
                None,
                None,
            )
        }
        .map_err(io::Error::from)?;
        let end = label.iter().position(|unit| *unit == 0).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "volume label is not terminated")
        })?;
        let units = label.get(..end).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid volume label length")
        })?;
        String::from_utf16(units)
            .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))
    }

    pub(super) fn free_bytes(&self) -> io::Result<u64> {
        let mut total_free = 0;
        // SAFETY: The HSTRING owns a NUL-terminated root path throughout the call. total_free is
        // a live, aligned writable u64 passed as lpTotalNumberOfFreeBytes; other outputs are null.
        unsafe { GetDiskFreeSpaceExW(PCWSTR(self.0.as_ptr()), None, None, Some(&mut total_free)) }
            .map_err(io::Error::from)?;
        Ok(total_free)
    }
}
