use std::fs::{File, Metadata};
use std::io;

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

#[cfg(unix)]
pub fn file_inode(_file: &File, metadata: &Metadata) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;

    Ok(metadata.ino())
}

#[cfg(windows)]
pub fn file_inode(file: &File, _metadata: &Metadata) -> io::Result<u64> {
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
    if unsafe { fileapi::GetFileInformationByHandle(handle, &mut info) } != 0 {
        Ok((info.nFileIndexHigh as u64) << 32 | (info.nFileIndexLow as u64))
    } else {
        Err(io::Error::last_os_error())
    }
}
