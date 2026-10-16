//! What differs between Windows and Unix: links that must not be followed, hard links, size on disk, file systems.
//!
//! Windows calls are declared here against kernel32, which every Windows has; no crate, no prebuilt import library.

use std::fs::Metadata;
use std::path::Path;

// Links are classified in fastdir (from the listing itself: the reparse tag comes with it on Windows). They are
// listed, never followed: following a junction counts a tree twice (2026-10-16: `Gorilla.firefox\src` is a
// junction to `Build.Work\firefox\157.0-truth`; a walk that followed it would report 4.5 GiB that exist once).

/// The file system (device) a directory lives on, to stay on one (Unix). Windows: mounted volumes inside a folder
/// are reparse points, so they are never entered anyway.
pub fn device(md: &Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(md.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        None
    }
}

/// (volume/device, file id) of a file that has more than one name, so it is counted once. None: a single name, or
/// not checked. `windows_check`: open the file to ask (Windows keeps link counts out of directory listings).
pub fn hardlink_id(path: &Path, md: &Metadata, windows_check: bool) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = (path, windows_check);
        if md.nlink() > 1 {
            return Some((md.dev(), md.ino()));
        }
        None
    }
    #[cfg(windows)]
    {
        let _ = md;
        if !windows_check {
            return None;
        }
        win::file_info(path).and_then(|i| if i.links > 1 { Some((i.volume as u64, i.index)) } else { None })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, md, windows_check);
        None
    }
}

/// Bytes the file really occupies on the disk. Unix: allocated blocks (free, it is in the stat already). Windows: the
/// compressed/sparse size rounded up to whole clusters (one extra call per file, so only with --disk).
pub fn disk_size(path: &Path, md: &Metadata, cluster: u64, windows_check: bool) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = (path, cluster, windows_check);
        md.blocks() * 512
    }
    #[cfg(windows)]
    {
        if !windows_check {
            return round_up(md.len(), cluster);
        }
        round_up(win::compressed_size(path).unwrap_or(md.len()), cluster)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, cluster, windows_check);
        md.len()
    }
}

/// A directory that is a reparse point (junction, mount point, directory symlink): never entered.
pub fn is_reparse_dir(md: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        md.is_dir() && md.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        md.file_type().is_symlink()
    }
}

pub fn round_up(n: u64, unit: u64) -> u64 {
    if unit <= 1 {
        n
    } else {
        n.div_ceil(unit) * unit
    }
}

/// Cluster (allocation unit) size of the volume holding `path`. Unix: the block size the stat reports per file is used
/// instead, so this is only for Windows. 4096 when it cannot be asked.
pub fn cluster_size(path: &Path) -> u64 {
    #[cfg(windows)]
    {
        win::cluster_size(path).unwrap_or(4096)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        4096
    }
}

#[cfg(windows)]
pub mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    pub type Handle = *mut c_void;
    pub const INVALID: Handle = -1isize as Handle;

    #[repr(C)]
    #[derive(Default)]
    struct ByHandleInfo {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume_serial: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateFileW(name: *const u16, access: u32, share: u32, sa: *mut c_void, disposition: u32, flags: u32,
                           template: Handle) -> Handle;
        pub fn CloseHandle(h: Handle) -> i32;
        fn GetFileInformationByHandle(h: Handle, info: *mut ByHandleInfo) -> i32;
        fn GetCompressedFileSizeW(name: *const u16, high: *mut u32) -> u32;
        fn GetLastError() -> u32;
        fn GetVolumePathNameW(name: *const u16, buf: *mut u16, len: u32) -> i32;
        fn GetDiskFreeSpaceW(root: *const u16, sectors_per_cluster: *mut u32, bytes_per_sector: *mut u32,
                             free: *mut u32, total: *mut u32) -> i32;
        fn GetCurrentProcess() -> Handle;
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn GetTokenInformation(token: Handle, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
    }

    /// Wide, NUL-terminated, with the \\?\ prefix when long (Windows' 260-character limit does not apply then).
    pub fn wide(p: &Path) -> Vec<u16> {
        let s = p.as_os_str();
        let mut v: Vec<u16> = Vec::with_capacity(s.len() + 8);
        let long = s.len() > 240 && !s.to_string_lossy().starts_with(r"\\");
        if long && p.is_absolute() {
            v.extend(r"\\?\".encode_utf16());
        }
        v.extend(s.encode_wide());
        v.push(0);
        v
    }

    pub struct Info {
        pub volume: u32,
        pub index: u64,
        pub links: u32,
    }

    pub fn file_info(p: &Path) -> Option<Info> {
        const READ_ATTRIBUTES: u32 = 0x80;
        const SHARE_ALL: u32 = 7;
        const OPEN_EXISTING: u32 = 3;
        const BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let w = wide(p);
        unsafe {
            let h = CreateFileW(w.as_ptr(), READ_ATTRIBUTES, SHARE_ALL, std::ptr::null_mut(), OPEN_EXISTING,
                                BACKUP_SEMANTICS | OPEN_REPARSE_POINT, std::ptr::null_mut());
            if h == INVALID {
                return None;
            }
            let mut i = ByHandleInfo::default();
            let ok = GetFileInformationByHandle(h, &mut i);
            CloseHandle(h);
            if ok == 0 {
                return None;
            }
            Some(Info { volume: i.volume_serial, index: ((i.index_high as u64) << 32) | i.index_low as u64,
                        links: i.links })
        }
    }

    pub fn compressed_size(p: &Path) -> Option<u64> {
        let w = wide(p);
        unsafe {
            let mut high = 0u32;
            let low = GetCompressedFileSizeW(w.as_ptr(), &mut high);
            if low == u32::MAX && GetLastError() != 0 {
                return None;
            }
            Some(((high as u64) << 32) | low as u64)
        }
    }

    pub fn volume_root(p: &Path) -> Option<Vec<u16>> {
        let w = wide(p);
        let mut buf = vec![0u16; 1024];
        unsafe {
            if GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) == 0 {
                return None;
            }
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        buf.truncate(n + 1);
        Some(buf)
    }

    /// The long form of a path: `C:\Users\<SHORT~1>\...` -> `C:\Users\<long name>\...`. 2026-10-16 (CI): --mft looks
    /// names up in the master file table by their long names, and a temp path holding an 8.3 short name was "not on
    /// C:". Unchanged when Windows cannot tell (the path does not exist).
    pub fn long_path(p: &Path) -> std::path::PathBuf {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetLongPathNameW(short: *const u16, long: *mut u16, len: u32) -> u32;
        }
        let w = wide(p);
        let mut buf = vec![0u16; 32768];
        let n = unsafe { GetLongPathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n == 0 || n >= buf.len() {
            return p.to_path_buf();
        }
        let s = String::from_utf16_lossy(&buf[..n]);
        std::path::PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
    }

    pub fn cluster_size(p: &Path) -> Option<u64> {
        let root = volume_root(p)?;
        let (mut spc, mut bps, mut f, mut t) = (0u32, 0u32, 0u32, 0u32);
        unsafe {
            if GetDiskFreeSpaceW(root.as_ptr(), &mut spc, &mut bps, &mut f, &mut t) == 0 {
                return None;
            }
        }
        Some(spc as u64 * bps as u64)
    }

    pub fn is_admin() -> bool {
        const TOKEN_QUERY: u32 = 8;
        const TOKEN_ELEVATION: u32 = 20;
        unsafe {
            let mut tok: Handle = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
                return false;
            }
            let mut elevated: u32 = 0;
            let mut ret = 0u32;
            let ok = GetTokenInformation(tok, TOKEN_ELEVATION, &mut elevated as *mut u32 as *mut c_void, 4, &mut ret);
            CloseHandle(tok);
            ok != 0 && elevated != 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_to_clusters() {
        assert_eq!(round_up(0, 4096), 0);
        assert_eq!(round_up(1, 4096), 4096);
        assert_eq!(round_up(4096, 4096), 4096);
        assert_eq!(round_up(4097, 4096), 8192);
        assert_eq!(round_up(5, 1), 5);
    }
}
