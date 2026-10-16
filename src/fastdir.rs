//! One directory listing, as fast as the operating system allows, with everything the walk needs per entry.
//!
//! Windows: FindFirstFileExW with FindExInfoBasic (no 8.3 short names to build) and FIND_FIRST_EX_LARGE_FETCH (bigger
//! batches per kernel call). The size, times, attributes and the reparse tag come with the listing: no call per file.
//! Unix: readdir + fstatat relative to the open directory (what std's DirEntry::metadata does), no follow.

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EKind {
    File,
    Dir,
    /// a link or reparse directory that is never entered; the str says which
    Link(&'static str),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: EKind,
    pub len: u64,
    /// seconds since 1970
    pub mtime: Option<u64>,
    /// Windows file attributes, or Unix st_mode
    pub attrs: u32,
    /// Unix: (dev, ino) when the file has more than one name; Windows: filled only when asked (--links)
    pub hardlink: Option<(u64, u64)>,
    /// Unix: the device, to stay on one file system
    pub dev: Option<u64>,
    /// Unix: allocated bytes (st_blocks * 512); Windows: None (measured on request)
    pub alloc: Option<u64>,
}

#[cfg(windows)]
mod imp {
    use super::{EKind, Entry};
    use std::ffi::c_void;
    use std::path::Path;

    #[repr(C)]
    struct FindData {
        attrs: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        size_high: u32,
        size_low: u32,
        reserved0: u32, // the reparse tag when FILE_ATTRIBUTE_REPARSE_POINT is set
        reserved1: u32,
        name: [u16; 260],
        alt: [u16; 14],
        // obsolete fields some SDKs declare after cAlternateFileName; padding keeps the struct large enough
        _pad: [u32; 4],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn FindFirstFileExW(name: *const u16, level: u32, data: *mut FindData, search_op: u32, filter: *mut c_void,
                            flags: u32) -> *mut c_void;
        fn FindNextFileW(h: *mut c_void, data: *mut FindData) -> i32;
        fn FindClose(h: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const FIND_EX_INFO_BASIC: u32 = 1;
    const FIND_EX_SEARCH_NAME_MATCH: u32 = 0;
    const DIRECTORY: u32 = 0x10;
    const REPARSE: u32 = 0x400;
    const TAG_MOUNT_POINT: u32 = 0xA000_0003;
    const TAG_SYMLINK: u32 = 0xA000_000C;
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_NO_MORE_FILES: u32 = 18;

    fn unix_secs(ft: [u32; 2]) -> Option<u64> {
        let t = ((ft[1] as u64) << 32) | ft[0] as u64;
        (t / 10_000_000).checked_sub(11_644_473_600)
    }

    pub fn list(dir: &Path) -> std::io::Result<Vec<Entry>> {
        let mut pat = crate::os::win::wide(&dir.join("*"));
        if !pat.starts_with(&[b'\\' as u16, b'\\' as u16]) && dir.as_os_str().len() > 200 && dir.is_absolute() {
            let mut v: Vec<u16> = r"\\?\".encode_utf16().collect();
            v.extend_from_slice(&pat);
            pat = v;
        }
        let mut fd: FindData = unsafe { std::mem::zeroed() };
        // NOT FIND_FIRST_EX_LARGE_FETCH. Measured 2026-10-16 on this laptop (NVMe, warm cache, 6 alternating runs):
        // 473,152-file tree large-fetch 0.820 s, basic 0.622 s, std::fs::read_dir 0.599 s; 3.2 million files 7.10 /
        // 6.14 / 6.40 s. Large fetch was ~30 % SLOWER. Basic stays: level with std, and it hands over the reparse tag.
        let h = unsafe {
            FindFirstFileExW(pat.as_ptr(), FIND_EX_INFO_BASIC, &mut fd, FIND_EX_SEARCH_NAME_MATCH,
                             std::ptr::null_mut(), 0)
        };
        if h == crate::os::win::INVALID {
            let e = unsafe { GetLastError() };
            if e == ERROR_FILE_NOT_FOUND {
                return Ok(Vec::new());
            }
            return Err(std::io::Error::from_raw_os_error(e as i32));
        }
        let mut out = Vec::new();
        loop {
            let n = fd.name.iter().position(|&c| c == 0).unwrap_or(260);
            let name = &fd.name[..n];
            let dot = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
            if !dot {
                let kind = if fd.attrs & REPARSE != 0 && fd.attrs & DIRECTORY != 0 {
                    EKind::Link(match fd.reserved0 {
                        TAG_MOUNT_POINT => "junction or mount point",
                        TAG_SYMLINK => "directory symlink",
                        _ => "reparse folder (not entered)",
                    })
                } else if fd.attrs & REPARSE != 0 && fd.reserved0 == TAG_SYMLINK {
                    EKind::Link("file symlink")
                } else if fd.attrs & DIRECTORY != 0 {
                    EKind::Dir
                } else {
                    EKind::File
                };
                let len = if kind == EKind::File { ((fd.size_high as u64) << 32) | fd.size_low as u64 } else { 0 };
                out.push(Entry { name: String::from_utf16_lossy(name), kind, len, mtime: unix_secs(fd.written),
                                 attrs: fd.attrs, hardlink: None, dev: None, alloc: None });
            }
            if unsafe { FindNextFileW(h, &mut fd) } == 0 {
                let e = unsafe { GetLastError() };
                unsafe { FindClose(h) };
                if e == ERROR_NO_MORE_FILES {
                    return Ok(out);
                }
                return Err(std::io::Error::from_raw_os_error(e as i32));
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::{EKind, Entry};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::time::UNIX_EPOCH;

    pub fn list(dir: &Path) -> std::io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        for ent in std::fs::read_dir(dir)? {
            let ent = ent?;
            let ft = ent.file_type()?;
            let name = ent.file_name().to_string_lossy().into_owned();
            if ft.is_symlink() {
                out.push(Entry { name, kind: EKind::Link("symlink"), len: 0, mtime: None, attrs: 0, hardlink: None,
                                 dev: None, alloc: None });
                continue;
            }
            let md = ent.metadata()?; // fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)
            let kind = if md.is_dir() { EKind::Dir } else { EKind::File };
            let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
            let hard = (kind == EKind::File && md.nlink() > 1).then(|| (md.dev(), md.ino()));
            out.push(Entry { name, kind, len: if kind == EKind::File { md.len() } else { 0 }, mtime, attrs: md.mode(),
                             hardlink: hard, dev: Some(md.dev()), alloc: Some(md.blocks() * 512) });
        }
        Ok(out)
    }
}

pub fn list(dir: &Path) -> std::io::Result<Vec<Entry>> {
    imp::list(dir)
}

/// Windows attribute letters, like `attrib` (R H S A C E O): a long listing without another call.
pub fn attr_letters(a: u32) -> String {
    #[cfg(windows)]
    {
        let mut s = String::new();
        for (bit, c) in [(0x1, 'R'), (0x2, 'H'), (0x4, 'S'), (0x20, 'A'), (0x800, 'C'), (0x4000, 'E'), (0x1000, 'O')] {
            s.push(if a & bit != 0 { c } else { '-' });
        }
        s
    }
    #[cfg(not(windows))]
    {
        // st_mode -> rwxr-xr-x
        let mut s = String::with_capacity(9);
        for shift in [6u32, 3, 0] {
            let b = (a >> shift) & 7;
            s.push(if b & 4 != 0 { 'r' } else { '-' });
            s.push(if b & 2 != 0 { 'w' } else { '-' });
            s.push(if b & 1 != 0 { 'x' } else { '-' });
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_files_folders_and_sizes() {
        let d = std::env::temp_dir().join(format!("gs-fastdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("a.txt"), b"hello").unwrap();
        let mut v = list(&d).unwrap();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].name.as_str(), v[0].kind, v[0].len), ("a.txt", EKind::File, 5));
        assert_eq!((v[1].name.as_str(), v[1].kind), ("sub", EKind::Dir));
        assert!(v[0].mtime.is_some());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_empty_folder_is_an_empty_list() {
        let d = std::env::temp_dir().join(format!("gs-fastdir-empty-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(list(&d).unwrap().is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
