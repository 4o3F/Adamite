#![allow(unsafe_code)]
#![deny(
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block
)]

use super::CommandLine;
use std::{io, mem::size_of, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{
            GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
        },
        UI::{
            Shell::{
                SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{PCWSTR, w},
};

#[derive(Debug)]
pub(super) struct ProcessHandle(HANDLE);

impl ProcessHandle {
    pub(super) fn is_current() -> io::Result<bool> {
        // SAFETY: GetCurrentProcess takes no arguments and returns a borrowed pseudo-handle.
        let process = unsafe { GetCurrentProcess() };
        let mut token = HANDLE::default();
        // SAFETY: process is the current process pseudo-handle and token is writable for the call.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(io::Error::from)?;
        let token = Self(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let size = u32::try_from(size_of::<TOKEN_ELEVATION>()).map_err(io::Error::other)?;
        let mut returned = 0;
        // SAFETY: token owns an open query handle. elevation is initialized, aligned and writable
        // for size bytes, and returned is writable until the synchronous call completes.
        unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                Some(ptr::from_mut(&mut elevation).cast()),
                size,
                &mut returned,
            )
        }
        .map_err(io::Error::from)?;
        if returned != size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete token elevation response",
            ));
        }
        Ok(elevation.TokenIsElevated != 0)
    }

    pub(super) fn launch(executable: &Path, parameters: &CommandLine) -> io::Result<Self> {
        let executable: Vec<u16> = executable
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let parameters: Vec<u16> = parameters
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>()).map_err(io::Error::other)?,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(executable.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: info is initialized with its correct size; its path, parameter and verb pointers
        // are NUL-terminated and live until the synchronous shell call completes. Unused fields
        // are zero. NOCLOSEPROCESS transfers ownership of the returned process handle to us.
        unsafe { ShellExecuteExW(&mut info) }.map_err(io::Error::from)?;
        if info.hProcess.is_invalid() {
            return Err(io::Error::other("shell did not return a process handle"));
        }
        Ok(Self(info.hProcess))
    }

    pub(super) fn try_wait(&self) -> io::Result<Option<u32>> {
        self.wait_for(0)
    }

    pub(super) fn wait(&self) -> io::Result<u32> {
        self.wait_for(INFINITE)?
            .ok_or_else(|| io::Error::other("infinite process wait timed out"))
    }

    fn wait_for(&self, timeout: u32) -> io::Result<Option<u32>> {
        // SAFETY: self owns a live process handle with synchronization access for the whole wait.
        let result = unsafe { WaitForSingleObject(self.0, timeout) };
        match result {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: self owns a live, signaled process handle and code is writable.
                unsafe { GetExitCodeProcess(self.0, &mut code) }.map_err(io::Error::from)?;
                Ok(Some(code))
            }
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("unexpected process wait result")),
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: self uniquely owns the real process or token handle, and closes it exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
