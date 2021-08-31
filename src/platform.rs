// Copyright (c) 2016-2021 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::fs::{File, Metadata};
use std::io;
use std::time::SystemTime;

pub trait FileExt {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl FileExt for std::fs::File {
    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        use std::os::unix::fs::FileExt;

        FileExt::read_at(self, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        use std::os::windows::fs::FileExt;

        // The difference between this and the Unix version is that `seek_read`
        // changes the file cursor position, while `read_at` doesn't.
        // We don't depend on the cursor, so that's all right.
        FileExt::seek_read(self, buf, offset)
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
