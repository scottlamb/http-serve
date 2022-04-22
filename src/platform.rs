// Copyright (c) 2016-2021 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::convert::TryFrom;
use std::fs::{File, Metadata};
use std::io;
use std::time::SystemTime;

pub trait FileExt {
    /// Reads at least 1, at most `chunk_size` bytes beginning at `offset`, or fails.
    ///
    /// If there are no bytes at `offset`, returns an `UnexpectedEof` error.
    ///
    /// The file cursor changes on Windows (like `std::os::windows::fs::seek_read`) but not Unix
    /// (like `std::os::unix::fs::FileExt::read_at`). The caller never uses the cursor, so this
    /// doesn't matter.
    ///
    /// The implementation goes directly to `libc` or `winapi` to allow soundly reading into an
    /// uninitialized buffer. This may change after
    /// [`read_buf`](https://github.com/rust-lang/rust/issues/78485) is stabilized, including buf
    /// equivalents of `read_at`/`seek_read`.
    fn read_at(&self, chunk_size: usize, offset: u64) -> io::Result<Vec<u8>>;
}

impl FileExt for std::fs::File {
    #[cfg(unix)]
    fn read_at(&self, chunk_size: usize, offset: u64) -> io::Result<Vec<u8>> {
        use std::os::unix::io::AsRawFd;

        let mut chunk = Vec::with_capacity(chunk_size);
        let offset = libc::off_t::try_from(offset).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset too large")
        })?;

        // SAFETY: `Vec::with_capacity` guaranteed the passed pointers are valid.
        let retval = unsafe {
            libc::pread(
                self.as_raw_fd(),
                chunk.as_mut_ptr() as *mut libc::c_void,
                chunk_size,
                offset,
            )
        };
        let bytes_read = usize::try_from(retval).map_err(|_| std::io::Error::last_os_error())?;

        if bytes_read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("no bytes beyond position {}", offset),
            ));
        }

        // SAFETY: `libc::pread` guaranteed these bytes are initialized.
        unsafe {
            chunk.set_len(bytes_read);
        }
        Ok(chunk)
    }

    #[cfg(windows)]
    fn read_at(&self, chunk_size: usize, offset: u64) -> io::Result<Vec<u8>> {
        use std::os::windows::io::AsRawHandle;
        use winapi::shared::minwindef::DWORD;
        let handle = self.as_raw_handle();
        let mut read = 0;
        let mut chunk = Vec::with_capacity(chunk_size);

        // https://github.com/rust-lang/rust/blob/5ffebc2cb3a089c27a4c7da13d09fd2365c288aa/library/std/src/sys/windows/handle.rs#L230
        // https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile
        unsafe {
            let mut overlapped: winapi::um::minwinbase::OVERLAPPED = std::mem::zeroed();
            overlapped.u.s_mut().Offset = offset as u32;
            overlapped.u.s_mut().OffsetHigh = (offset >> 32) as u32;

            // SAFETY: `Vec::with_capacity` guaranteed the pointer range is valid.
            if winapi::um::fileapi::ReadFile(
                handle,
                chunk.as_mut_ptr() as *mut winapi::ctypes::c_void,
                DWORD::try_from(chunk_size).unwrap_or(DWORD::MAX), // saturating conversion
                &mut read,
                &mut overlapped,
            ) == 0
            {
                match winapi::um::errhandlingapi::GetLastError() {
                    // Match std's <https://github.com/rust-lang/rust/issues/81357> fix:
                    // abort the process before `overlapped` is dropped.
                    #[allow(clippy::print_stderr)]
                    winapi::shared::winerror::ERROR_IO_PENDING => {
                        eprintln!("I/O error: operation failed to complete synchronously");
                        std::process::abort();
                    }
                    winapi::shared::winerror::ERROR_HANDLE_EOF => {
                        // std::io::Error::from_raw_os_error converts this to ErrorKind::Other.
                        // Override that.
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            format!("no bytes beyond position {}", offset),
                        ));
                    }
                    o => return Err(std::io::Error::from_raw_os_error(o as i32)),
                }
            }

            // SAFETY: `ReadFile` guaranteed that `read` bytes have been read.
            chunk.set_len(usize::try_from(read).expect("u32 should fit in usize"));
        }
        Ok(chunk)
    }
}

pub struct FileInfo {
    pub inode: u64,
    pub len: u64,
    pub mtime: SystemTime,
}

#[cfg(windows)]
/// Converts a Windows `FILETIME` to a Rust `SystemTime`
///
/// `FILETIME` is the number of 100 ns ticks since Jan 1 1601.
/// Unix time is the number of seconds since Jan 1 1970.
fn filetime_to_systemtime(time: winapi::shared::minwindef::FILETIME) -> SystemTime {
    use std::time::{Duration, UNIX_EPOCH};

    let ticks = (time.dwHighDateTime as u64) << 32 | time.dwLowDateTime as u64;

    // Number of seconds between the Windows and the Unix epoch
    const SECS_TO_UNIX_EPOCH: u64 = 11_644_473_600;
    let secs = ticks / 10_000_000 - SECS_TO_UNIX_EPOCH;
    let nanos = (ticks % 10_000_000 * 100) as u32;

    let duration = Duration::new(secs, nanos);
    UNIX_EPOCH + duration
}

#[cfg(unix)]
pub fn file_info(_file: &File, metadata: &Metadata) -> io::Result<FileInfo> {
    use std::os::unix::fs::MetadataExt;

    let info = FileInfo {
        inode: metadata.ino(),
        len: metadata.len(),
        mtime: metadata.modified()?,
    };

    Ok(info)
}

// TODO: switch to using std::os::windows::fs::MetadataExt when the accessors
// we need are stable: https://github.com/rust-lang/rust/issues/63010
// This will reduce the number of system calls and eliminate the winapi crate dependency.
#[cfg(windows)]
pub fn file_info(file: &File, _metadata: &Metadata) -> io::Result<FileInfo> {
    use std::os::windows::io::AsRawHandle;
    use winapi::shared::minwindef::FILETIME;
    use winapi::um::fileapi::{self, BY_HANDLE_FILE_INFORMATION};

    let handle = file.as_raw_handle();
    let zero_time = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION {
        dwFileAttributes: 0,
        ftCreationTime: zero_time,
        ftLastAccessTime: zero_time,
        ftLastWriteTime: zero_time,
        dwVolumeSerialNumber: 0,
        nFileSizeHigh: 0,
        nFileSizeLow: 0,
        nNumberOfLinks: 0,
        nFileIndexHigh: 0,
        nFileIndexLow: 0,
    };

    let inode = if unsafe { fileapi::GetFileInformationByHandle(handle, &mut info) } != 0 {
        (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64
    } else {
        return Err(io::Error::last_os_error());
    };
    let mtime = filetime_to_systemtime(info.ftLastWriteTime);
    let len = (info.nFileSizeHigh as u64) << 32 | info.nFileSizeLow as u64;

    let info = FileInfo { inode, len, mtime };
    Ok(info)
}
