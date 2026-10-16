//! hash, dupes, empty, reparse, free: the read-only questions a cleanup asks after "how big".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::fastdir::{self, EKind};
use crate::sha256;

/// SHA-256 of every file given (folders: every file under them), in parallel.
pub fn hash(paths: &[PathBuf]) -> Vec<(String, Result<String, String>, u64)> {
    let mut files = Vec::new();
    for p in paths {
        collect_files(p, &mut files);
    }
    let mut out: Vec<_> = files.par_iter().map(|(p, len)| {
        (p.display().to_string(), sha256::file(p, None).map(|d| sha256::hex(&d)).map_err(|e| e.to_string()), *len)
    }).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn collect_files(p: &Path, out: &mut Vec<(PathBuf, u64)>) {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() && !crate::os::is_reparse_dir(&m) => {
            if let Ok(v) = fastdir::list(p) {
                for e in v {
                    let c = p.join(&e.name);
                    match e.kind {
                        EKind::File => out.push((c, e.len)),
                        EKind::Dir => collect_files(&c, out),
                        EKind::Link(_) => {}
                    }
                }
            }
        }
        Ok(m) if m.is_file() => out.push((p.to_path_buf(), m.len())),
        _ => {}
    }
}

pub struct DupeGroup {
    pub bytes: u64,
    pub hash: String,
    pub paths: Vec<String>,
}

/// Same bytes in more than one place: grouped by size, then by the hash of the first 64 KiB, then by the full
/// hash. Only files of at least `min` bytes. Every candidate is read at most twice; unique sizes are never read.
pub fn dupes(roots: &[PathBuf], min: u64) -> Vec<DupeGroup> {
    let mut files = Vec::new();
    for r in roots {
        collect_files(r, &mut files);
    }
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for (p, len) in files {
        if len >= min.max(1) {
            by_size.entry(len).or_default().push(p);
        }
    }
    let groups: Vec<(u64, Vec<PathBuf>)> = by_size.into_iter().filter(|(_, v)| v.len() > 1).collect();
    let mut out: Vec<DupeGroup> = groups.into_par_iter().flat_map(|(len, paths)| {
        let quick = |p: &PathBuf| sha256::file(p, Some(64 * 1024)).ok().map(|d| sha256::hex(&d));
        let mut by_quick: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for p in paths {
            if let Some(h) = quick(&p) {
                by_quick.entry(h).or_default().push(p);
            }
        }
        let mut found = Vec::new();
        for (_, ps) in by_quick.into_iter().filter(|(_, v)| v.len() > 1) {
            let mut by_full: HashMap<String, Vec<String>> = HashMap::new();
            for p in ps {
                let h = if len <= 64 * 1024 { quick(&p) } else { sha256::file(&p, None).ok().map(|d| sha256::hex(&d)) };
                if let Some(h) = h {
                    by_full.entry(h).or_default().push(p.display().to_string());
                }
            }
            for (h, mut v) in by_full.into_iter().filter(|(_, v)| v.len() > 1) {
                v.sort();
                found.push(DupeGroup { bytes: len, hash: h, paths: v });
            }
        }
        found
    }).collect();
    out.sort_by(|a, b| (b.bytes * (b.paths.len() as u64 - 1)).cmp(&(a.bytes * (a.paths.len() as u64 - 1)))
        .then_with(|| a.paths.cmp(&b.paths)));
    out
}

/// Empty folders (nothing inside, or only empty folders) and zero-byte files.
pub fn empty(root: &Path) -> (Vec<String>, Vec<String>) {
    fn go(d: &Path, dirs: &mut Vec<String>, files: &mut Vec<String>) -> bool {
        let v = match fastdir::list(d) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let mut all_empty = true;
        for e in v {
            let c = d.join(&e.name);
            match e.kind {
                EKind::File => {
                    all_empty = false;
                    if e.len == 0 {
                        files.push(c.display().to_string());
                    }
                }
                EKind::Dir => {
                    if !go(&c, dirs, files) {
                        all_empty = false;
                    }
                }
                EKind::Link(_) => all_empty = false,
            }
        }
        if all_empty {
            dirs.push(d.display().to_string());
        }
        all_empty
    }
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    go(root, &mut dirs, &mut files);
    dirs.retain(|d| Path::new(d) != root);
    dirs.sort();
    files.sort();
    (dirs, files)
}

/// Every link under `root`: what it is, where it points, whether that exists.
pub fn reparse(root: &Path) -> Vec<(String, String, String, bool)> {
    let rep = crate::walk::run(root, crate::walk::Opts::totals_only());
    rep.links.into_iter().map(|s| {
        let t = std::fs::read_link(&s.path).map(|t| t.display().to_string()).unwrap_or_else(|_| "?".into());
        let target_abs = std::fs::read_link(&s.path).ok().map(|t| {
            if t.is_absolute() { t } else { Path::new(&s.path).parent().unwrap_or(Path::new("")).join(t) }
        });
        let ok = target_abs.map(|t| t.exists()).unwrap_or(false);
        (s.path, s.why, t, ok)
    }).collect()
}

/// (mount or drive, kind, total bytes, free bytes)
pub fn free() -> Vec<(String, String, u64, u64)> {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetLogicalDriveStringsW(len: u32, buf: *mut u16) -> u32;
            fn GetDiskFreeSpaceExW(root: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        }
        let mut buf = vec![0u16; 512];
        let n = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
        let s = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        let mut out = Vec::new();
        for d in s.split('\0').filter(|d| !d.is_empty()) {
            let w: Vec<u16> = d.encode_utf16().chain(Some(0)).collect();
            let (mut avail, mut total, mut fr) = (0u64, 0u64, 0u64);
            if unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut fr) } != 0 {
                let (_, kind) = crate::profile::volume(Path::new(d));
                out.push((d.to_string(), kind, total, avail));
            }
        }
        out
    }
    #[cfg(all(unix, target_pointer_width = "64"))]
    {
        #[repr(C)]
        struct StatVfs { bsize: u64, frsize: u64, blocks: u64, bfree: u64, bavail: u64, files: u64, ffree: u64,
                         favail: u64, fsid: u64, flag: u64, namemax: u64, spare: [i32; 6] }
        extern "C" {
            fn statvfs(path: *const std::ffi::c_char, buf: *mut StatVfs) -> i32;
        }
        let real = ["ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk", "zfs"];
        let mut out = Vec::new();
        let text = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        for l in text.lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 3 || !real.contains(&f[2]) {
                continue;
            }
            let c = std::ffi::CString::new(f[1]).unwrap();
            let mut s: StatVfs = unsafe { std::mem::zeroed() };
            if unsafe { statvfs(c.as_ptr(), &mut s) } == 0 {
                let (_, kind) = crate::profile::volume(Path::new(f[1]));
                out.push((f[1].to_string(), kind, s.blocks * s.frsize, s.bavail * s.frsize));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_are_found_and_unique_sizes_are_not_read() {
        let d = std::env::temp_dir().join(format!("gs-dupes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        std::fs::write(d.join("a/one.bin"), vec![7u8; 100_000]).unwrap();
        std::fs::write(d.join("b/copy.bin"), vec![7u8; 100_000]).unwrap();
        let mut differs = vec![7u8; 100_000];
        differs[99_999] = 8; // same size, same first 64 KiB, different end: must NOT be a duplicate
        std::fs::write(d.join("b/almost.bin"), differs).unwrap();
        std::fs::write(d.join("unique.bin"), vec![1u8; 5]).unwrap();
        let g = dupes(&[d.clone()], 1);
        assert_eq!(g.len(), 1, "{:?}", g.iter().map(|x| &x.paths).collect::<Vec<_>>());
        assert_eq!(g[0].paths.len(), 2);
        assert!(g[0].paths.iter().any(|p| p.ends_with("one.bin")) && g[0].paths.iter().any(|p| p.ends_with("copy.bin")));
        let h = hash(&[d.join("unique.bin")]);
        let mut want = sha256::Sha256::default();
        want.update(&[1u8; 5]);
        assert_eq!(h[0].1.as_deref().unwrap(), sha256::hex(&want.finish()));
        assert_eq!(h[0].2, 5);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn empty_folders_and_zero_byte_files() {
        let d = std::env::temp_dir().join(format!("gs-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("e1/e2")).unwrap();
        std::fs::create_dir_all(d.join("full")).unwrap();
        std::fs::write(d.join("full/x"), b"x").unwrap();
        std::fs::write(d.join("zero"), b"").unwrap();
        let (dirs, files) = empty(&d);
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert!(dirs.iter().all(|x| x.contains("e1")));
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
