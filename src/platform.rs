use std::fs::File;
use std::io;
use std::time::SystemTime;

pub trait FileExt {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl FileExt for ::std::fs::File {
    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        use std::os::unix::fs::FileExt;

        FileExt::read_at(self, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        use std::os::windows::fs::FileExt;

        FileExt::seek_read(self, buf, offset)
    }
}

pub struct FileInfo {
    pub inode: u64,
    pub len: u64,
    pub mtime: SystemTime,
}

#[cfg(windows)]
fn filetime_to_systemtime(time: ::winapi::shared::minwindef::FILETIME) -> SystemTime {
    use std::time::{Duration, UNIX_EPOCH};

    let ticks = (time.dwHighDateTime as u64) << 32 | time.dwLowDateTime as u64;

    const SECS_TO_UNIX_EPOCH: u64 = 11_644_473_600;
    let secs = ticks / 10_000_000 - SECS_TO_UNIX_EPOCH;
    let nanos = (ticks % 10_000_000 * 100) as u32;

    let duration = Duration::new(secs, nanos);
    UNIX_EPOCH + duration
}

#[cfg(unix)]
pub fn file_info(file: &File) -> io::Result<FileInfo> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    let info = FileInfo {
        inode: metadata.ino(),
        len: metadata.len(),
        mtime: metadata.modified()?,
    };

    Ok(info)
}

#[cfg(windows)]
pub fn file_info(file: &File) -> io::Result<FileInfo> {
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
