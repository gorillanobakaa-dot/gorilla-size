//! copy, move, recycle, undo. Never a permanent delete (agreed 2026-10-16: emptying the bin is a person's click).
//!
//! Every action is verified at the moment it happens and written to a journal before the next one starts:
//!   copy     - every file's bytes compared with its source
//!   move     - same volume: renamed, then the target is there and the source is not; across volumes: verified copy,
//!              then the source is recycled (verified)
//!   recycle  - Windows: refused BEFORE acting when Windows would destroy instead of recycle (no bin on that drive,
//!              NukeOnDelete, bigger than the bin, paths over 259 characters); after acting, the item must be found
//!              in the bin's own records ($Recycle.Bin\<SID>\$I...). Linux: freedesktop trash, verified in it.
//!   undo     - a journal, backwards.
//! Born from 2026-10-16: a folder sent "to the Recycle Bin" with confirmations suppressed was destroyed instead, and
//! the check that said "recycled" only looked at whether the folder had left its place.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::content::same_bytes;

pub fn state_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("gorilla")
}

pub fn stamp() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}-{:03}", t.as_secs(), t.subsec_millis())
}

pub struct Journal {
    pub path: PathBuf,
    f: fs::File,
}

impl Journal {
    pub fn open(op: &str) -> std::io::Result<Journal> {
        let d = state_dir().join("journal");
        fs::create_dir_all(&d)?;
        let path = d.join(format!("{}-{op}.jsonl", stamp()));
        let f = fs::OpenOptions::new().create_new(true).append(true).open(&path)?;
        Ok(Journal { path, f })
    }
    pub fn write(&mut self, v: &Value) -> std::io::Result<()> {
        writeln!(self.f, "{v}")?;
        self.f.sync_data()
    }
}

#[derive(Debug)]
pub struct Done {
    pub op: &'static str,
    pub from: String,
    pub to: String,
    pub ok: bool,
    pub why: String,
}

fn done(op: &'static str, from: &Path, to: &Path, ok: bool, why: impl Into<String>) -> Done {
    Done { op, from: from.display().to_string(), to: to.display().to_string(), ok, why: why.into() }
}

/// (files, bytes) under a path, links not followed
pub fn tally(p: &Path) -> (u64, u64) {
    let md = match fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(_) => return (0, 0),
    };
    if !md.is_dir() || md.file_type().is_symlink() {
        return (1, md.len());
    }
    let (mut n, mut b) = (0, 0);
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let (a, c) = tally(&e.path());
            n += a;
            b += c;
        }
    }
    (n, b)
}

// ---------------------------------------------------------------------------------------------------------- copy

fn copy_tree(src: &Path, dst: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<(), String> {
    let md = fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if md.file_type().is_symlink() || crate::os::is_reparse_dir(&md) {
        return Err(format!("{}: a link; not copied (copy its target instead)", src.display()));
    }
    if md.is_dir() {
        fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        let rd = fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?;
        for e in rd {
            let e = e.map_err(|e| e.to_string())?;
            copy_tree(&e.path(), &dst.join(e.file_name()), out)?;
        }
    } else {
        fs::copy(src, dst).map_err(|e| format!("{} -> {}: {e}", src.display(), dst.display()))?;
        out.push((src.to_path_buf(), dst.to_path_buf()));
    }
    Ok(())
}

/// Copy `src` into the folder `to`. An existing target is refused unless `overwrite`, which first moves the old
/// one aside (`<name>.gorilla-bak-<stamp>`), never destroys it.
pub fn copy(src: &Path, to: &Path, overwrite: bool, j: &mut Journal) -> Done {
    let name = match src.file_name() {
        Some(n) => n,
        None => return done("copy", src, to, false, "no file name"),
    };
    let dst = to.join(name);
    if dst.exists() {
        if !overwrite {
            return done("copy", src, &dst, false, "target exists (use --overwrite: the old one is kept aside)");
        }
        let bak = dst.with_file_name(format!("{}.gorilla-bak-{}", name.to_string_lossy(), stamp()));
        if let Err(e) = fs::rename(&dst, &bak) {
            return done("copy", src, &dst, false, format!("could not move the old target aside: {e}"));
        }
        let _ = j.write(&json!({"op": "aside", "from": dst.display().to_string(), "to": bak.display().to_string()}));
    }
    let mut files = Vec::new();
    if let Err(e) = copy_tree(src, &dst, &mut files) {
        let _ = j.write(&json!({"op": "copy", "from": src.display().to_string(), "to": dst.display().to_string(),
                                "ok": false, "why": e}));
        return done("copy", src, &dst, false, e);
    }
    for (a, b) in &files {
        match same_bytes(a, b) {
            Ok(true) => {}
            Ok(false) => return done("copy", a, b, false, "VERIFY FAILED: the copy differs from its source"),
            Err(e) => return done("copy", a, b, false, format!("VERIFY FAILED: {e}")),
        }
    }
    let _ = j.write(&json!({"op": "copy", "from": src.display().to_string(), "to": dst.display().to_string(),
                            "ok": true, "files": files.len()}));
    done("copy", src, &dst, true, format!("{} file(s), every byte compared", files.len()))
}

// ---------------------------------------------------------------------------------------------------------- move

fn same_volume(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        let r = |p: &Path| crate::os::win::volume_root(p).map(|v| String::from_utf16_lossy(&v).to_lowercase());
        r(a).is_some() && r(a) == r(b)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::symlink_metadata(a), fs::metadata(b)) {
            (Ok(x), Ok(y)) => x.dev() == y.dev(),
            _ => false,
        }
    }
}

pub fn mv(src: &Path, to: &Path, j: &mut Journal) -> Done {
    let name = match src.file_name() {
        Some(n) => n,
        None => return done("move", src, to, false, "no file name"),
    };
    let dst = to.join(name);
    if dst.exists() {
        return done("move", src, &dst, false, "target exists; nothing moved");
    }
    let (n, bytes) = tally(src);
    if same_volume(src, to) {
        if let Err(e) = fs::rename(src, &dst) {
            return done("move", src, &dst, false, format!("{e}"));
        }
        let (n2, b2) = tally(&dst);
        if src.exists() || n2 != n || b2 != bytes {
            return done("move", src, &dst, false, format!("VERIFY FAILED: after the rename the target holds {n2} \
                         file(s)/{b2} bytes, expected {n}/{bytes}"));
        }
        let _ = j.write(&json!({"op": "move", "from": src.display().to_string(), "to": dst.display().to_string(),
                                "method": "rename", "ok": true}));
        return done("move", src, &dst, true, format!("renamed; {n} file(s) checked"));
    }
    let c = copy(src, to, false, j);
    if !c.ok {
        return done("move", src, &dst, false, format!("copy step: {}", c.why));
    }
    let r = recycle(src, j);
    if !r.ok {
        return done("move", src, &dst, false, format!("copied and verified, but the source could not be recycled: {}",
                                                     r.why));
    }
    let _ = j.write(&json!({"op": "move", "from": src.display().to_string(), "to": dst.display().to_string(),
                            "method": "copy+recycle", "ok": true}));
    done("move", src, &dst, true, format!("copied (every byte compared), source recycled ({})", r.to))
}

// ------------------------------------------------------------------------------------------------------- recycle

pub fn recycle(p: &Path, j: &mut Journal) -> Done {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    if fs::symlink_metadata(&abs).is_err() {
        return done("recycle", &abs, Path::new(""), false, "does not exist");
    }
    match bin::check(&abs) {
        Err(why) => {
            let _ = j.write(&json!({"op": "recycle", "from": abs.display().to_string(), "ok": false, "refused": why}));
            done("recycle", &abs, Path::new(""), false, format!("REFUSED, nothing touched: {why}"))
        }
        Ok(()) => match bin::send(&abs) {
            Ok(item) => {
                let _ = j.write(&json!({"op": "recycle", "from": abs.display().to_string(),
                                        "bin_item": item.display().to_string(), "ok": true}));
                done("recycle", &abs, &item, true, "found in the bin's own records")
            }
            Err(why) => {
                let _ = j.write(&json!({"op": "recycle", "from": abs.display().to_string(), "ok": false, "why": why}));
                done("recycle", &abs, Path::new(""), false, why)
            }
        },
    }
}

// ---------------------------------------------------------------------------------------------------------- undo

/// Reverse a journal, newest entry first. Copies are recycled (never deleted); moves go back; recycled items are
/// restored from the bin; an "aside" (an old target kept by --overwrite) is put back.
pub fn undo(journal: &Path) -> Vec<Done> {
    let text = match fs::read_to_string(journal) {
        Ok(t) => t,
        Err(e) => return vec![done("undo", journal, Path::new(""), false, e.to_string())],
    };
    let mut j = match Journal::open("undo") {
        Ok(j) => j,
        Err(e) => return vec![done("undo", journal, Path::new(""), false, e.to_string())],
    };
    let mut out = Vec::new();
    let entries: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    for e in entries.iter().rev() {
        if e["ok"] == false {
            continue;
        }
        let from = PathBuf::from(e["from"].as_str().unwrap_or(""));
        let to = PathBuf::from(e["to"].as_str().unwrap_or(""));
        let d = match e["op"].as_str() {
            Some("copy") => {
                let r = recycle(&to, &mut j);
                done("undo copy", &to, Path::new(&r.to), r.ok, r.why)
            }
            Some("move") if e["method"] == "rename" => match fs::rename(&to, &from) {
                Ok(()) => done("undo move", &to, &from, from.exists() && !to.exists(), "renamed back"),
                Err(err) => done("undo move", &to, &from, false, err.to_string()),
            },
            Some("move") => done("undo move", &to, &from, true,
                                 "across volumes: undone by the copy and recycle entries that follow"),
            Some("recycle") => {
                let item = PathBuf::from(e["bin_item"].as_str().unwrap_or(""));
                match bin::restore(&item, &from) {
                    Ok(()) => done("undo recycle", &item, &from, from.exists(), "restored from the bin"),
                    Err(err) => done("undo recycle", &item, &from, false, err),
                }
            }
            Some("aside") => match fs::rename(&to, &from) {
                Ok(()) => done("undo aside", &to, &from, true, "old target put back"),
                Err(err) => done("undo aside", &to, &from, false, err.to_string()),
            },
            _ => continue,
        };
        let _ = j.write(&json!({"op": d.op, "from": d.from, "to": d.to, "ok": d.ok, "why": d.why}));
        out.push(d);
    }
    out
}

// ------------------------------------------------------------------------------------------- the bin, per system

#[cfg(windows)]
mod bin {
    use std::ffi::c_void;
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::os::win::{self, CloseHandle, Handle};

    #[repr(C)]
    struct FileOp {
        hwnd: *mut c_void,
        func: u32,
        from: *const u16,
        to: *const u16,
        flags: u16,
        aborted: i32,
        mappings: *mut c_void,
        title: *const u16,
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHFileOperationW(op: *mut FileOp) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(root: *const u16) -> u32;
        fn GetVolumeNameForVolumeMountPointW(mount: *const u16, name: *mut u16, len: u32) -> i32;
        fn GetCurrentProcess() -> Handle;
        fn LocalFree(p: *mut c_void) -> *mut c_void;
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(key: isize, sub: *const u16, value: *const u16, flags: u32, typ: *mut u32, data: *mut c_void,
                        len: *mut u32) -> i32;
        fn OpenProcessToken(p: Handle, access: u32, tok: *mut Handle) -> i32;
        fn GetTokenInformation(tok: Handle, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
        fn ConvertSidToStringSidW(sid: *mut c_void, out: *mut *mut u16) -> i32;
    }

    const HKCU: isize = 0x8000_0001u32 as i32 as isize;
    const RRF_RT_DWORD: u32 = 0x18;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn root(p: &Path) -> Result<String, String> {
        let v = win::volume_root(p).ok_or("cannot tell which drive it is on")?;
        Ok(String::from_utf16_lossy(&v[..v.len() - 1]))
    }

    fn reg_dword(sub: &str, value: &str) -> Option<u32> {
        let (s, v) = (w(sub), w(value));
        let mut data = 0u32;
        let mut len = 4u32;
        let rc = unsafe {
            RegGetValueW(HKCU, s.as_ptr(), v.as_ptr(), RRF_RT_DWORD, std::ptr::null_mut(),
                         &mut data as *mut u32 as *mut c_void, &mut len)
        };
        (rc == 0).then_some(data)
    }

    fn sid() -> Result<String, String> {
        unsafe {
            let mut tok: Handle = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), 8, &mut tok) == 0 {
                return Err("cannot read this account's identity".into());
            }
            let mut buf = vec![0u8; 256];
            let mut ret = 0u32;
            let ok = GetTokenInformation(tok, 1, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut ret);
            CloseHandle(tok);
            if ok == 0 {
                return Err("cannot read this account's identity".into());
            }
            let sidp = *(buf.as_ptr() as *const *mut c_void);
            let mut s: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(sidp, &mut s) == 0 {
                return Err("cannot read this account's identity".into());
            }
            let mut n = 0;
            while *s.add(n) != 0 {
                n += 1;
            }
            let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s as *mut c_void);
            Ok(out)
        }
    }

    fn longest_path(p: &Path) -> usize {
        let mut m = p.as_os_str().len();
        if let Ok(md) = fs::symlink_metadata(p) {
            if md.is_dir() && !crate::os::is_reparse_dir(&md) {
                if let Ok(rd) = fs::read_dir(p) {
                    for e in rd.flatten() {
                        m = m.max(longest_path(&e.path()));
                    }
                }
            }
        }
        m
    }

    /// Everything that makes Windows destroy instead of recycle, checked before anything is touched.
    pub fn check(p: &Path) -> Result<(), String> {
        let r = root(p)?;
        let t = unsafe { GetDriveTypeW(w(&r).as_ptr()) };
        if t != 3 {
            return Err(format!("{r} is not a fixed drive (type {t}): Windows has no Recycle Bin there and would \
                                delete permanently"));
        }
        let mut vname = vec![0u16; 64];
        let ok = unsafe { GetVolumeNameForVolumeMountPointW(w(&r).as_ptr(), vname.as_mut_ptr(), vname.len() as u32) };
        if ok == 0 {
            return Err(format!("cannot identify the volume of {r}"));
        }
        let vn = String::from_utf16_lossy(&vname[..vname.iter().position(|&c| c == 0).unwrap_or(0)]);
        let guid = vn.trim_start_matches(r"\\?\Volume").trim_end_matches('\\').to_string();
        let key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
        if reg_dword(&key, "NukeOnDelete") == Some(1) {
            return Err(format!("{r} is set to delete immediately, without the Recycle Bin (NukeOnDelete)"));
        }
        let (_, bytes) = super::tally(p);
        if let Some(mb) = reg_dword(&key, "MaxCapacity") {
            if bytes > mb as u64 * 1024 * 1024 {
                return Err(format!("{} bytes is more than the Recycle Bin of {r} holds ({mb} MB): Windows would \
                                    delete it permanently", bytes));
            }
        }
        let lp = longest_path(p);
        if lp > 259 {
            return Err(format!("a path inside is {lp} characters long; the Recycle Bin cannot take paths over 259, \
                                and Windows may delete instead"));
        }
        Ok(())
    }

    /// The $I record names the original path: version 2 (Windows 10+) has a length then the name; version 1 a
    /// fixed 260-character name.
    fn original_of(i_file: &Path) -> Option<String> {
        let b = fs::read(i_file).ok()?;
        if b.len() < 28 {
            return None;
        }
        let ver = u64::from_le_bytes(b[0..8].try_into().ok()?);
        let (start, n) = if ver == 2 {
            (28usize, u32::from_le_bytes(b[24..28].try_into().ok()?) as usize)
        } else {
            (24usize, 260usize)
        };
        let chars: Vec<u16> = b.get(start..start + n * 2)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0).collect();
        Some(String::from_utf16_lossy(&chars))
    }

    fn bin_dir(p: &Path) -> Result<PathBuf, String> {
        Ok(PathBuf::from(root(p)?).join("$Recycle.Bin").join(sid()?))
    }

    fn find_in_bin(p: &Path, since: std::time::SystemTime) -> Result<Option<PathBuf>, String> {
        let dir = bin_dir(p)?;
        let want = p.display().to_string().to_lowercase();
        let rd = fs::read_dir(&dir).map_err(|e| format!("cannot read the Recycle Bin ({}): {e}", dir.display()))?;
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("$I") {
                continue;
            }
            let fresh = e.metadata().and_then(|m| m.modified()).map(|t| t >= since).unwrap_or(false);
            if fresh && original_of(&e.path()).map(|o| o.to_lowercase()) == Some(want.clone()) {
                return Ok(Some(dir.join(format!("$R{}", &name[2..]))));
            }
        }
        Ok(None)
    }

    pub fn send(p: &Path) -> Result<PathBuf, String> {
        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(2);
        let mut from: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().collect();
        from.extend([0, 0]);
        const FO_DELETE: u32 = 3;
        const FLAGS: u16 = 0x40 | 0x10 | 0x4 | 0x400; // ALLOWUNDO NOCONFIRMATION SILENT NOERRORUI
        let mut op = FileOp { hwnd: std::ptr::null_mut(), func: FO_DELETE, from: from.as_ptr(), to: std::ptr::null(),
                              flags: FLAGS, aborted: 0, mappings: std::ptr::null_mut(), title: std::ptr::null() };
        let rc = unsafe { SHFileOperationW(&mut op) };
        if rc != 0 || op.aborted != 0 {
            return Err(format!("Windows refused (code {rc:#x}); nothing was recycled"));
        }
        match find_in_bin(p, since)? {
            Some(item) if item.exists() => Ok(item),
            _ if p.exists() => Err("Windows reported success but the item is still in place; not recycled".into()),
            _ => Err("PERMANENTLY DELETED BY WINDOWS: it left its place and is NOT in the Recycle Bin. Stop and tell \
                      the person.".into()),
        }
    }

    pub fn restore(item: &Path, original: &Path) -> Result<(), String> {
        if original.exists() {
            return Err(format!("{} exists again; restore by hand from the Recycle Bin", original.display()));
        }
        if let Some(parent) = original.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::rename(item, original).map_err(|e| e.to_string())?;
        let name = item.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(rest) = name.strip_prefix("$R") {
            // the bin's own 500-byte record of the item; Explorer's restore removes it the same way
            let _ = fs::remove_file(item.with_file_name(format!("$I{rest}")));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_bins_records_are_read_in_both_formats() {
            let d = std::env::temp_dir().join(format!("gs-ifile-{}", std::process::id()));
            fs::create_dir_all(&d).unwrap();
            let path = r"D:\Work\archive\old.zip";
            let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
            let mut v2 = Vec::new();
            v2.extend(2u64.to_le_bytes());
            v2.extend(123u64.to_le_bytes());
            v2.extend(0u64.to_le_bytes());
            v2.extend((wide.len() as u32).to_le_bytes());
            v2.extend(wide.iter().flat_map(|c| c.to_le_bytes()));
            fs::write(d.join("v2"), &v2).unwrap();
            assert_eq!(original_of(&d.join("v2")).as_deref(), Some(path));
            let mut v1 = Vec::new();
            v1.extend(1u64.to_le_bytes());
            v1.extend(123u64.to_le_bytes());
            v1.extend(0u64.to_le_bytes());
            let mut fixed = wide.clone();
            fixed.resize(260, 0);
            v1.extend(fixed.iter().flat_map(|c| c.to_le_bytes()));
            fs::write(d.join("v1"), &v1).unwrap();
            assert_eq!(original_of(&d.join("v1")).as_deref(), Some(path));
            fs::remove_dir_all(&d).unwrap();
        }
    }
}

#[cfg(unix)]
mod bin {
    //! freedesktop.org Trash specification: $XDG_DATA_HOME/Trash/{files,info}, same file system only.
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    fn trash() -> Option<PathBuf> {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|d| d.join("Trash"))
    }

    pub fn check(p: &Path) -> Result<(), String> {
        let t = trash().ok_or("no home folder: no trash")?;
        fs::create_dir_all(t.join("files")).map_err(|e| e.to_string())?;
        fs::create_dir_all(t.join("info")).map_err(|e| e.to_string())?;
        let a = fs::symlink_metadata(p).map_err(|e| e.to_string())?.dev();
        let b = fs::metadata(t.join("files")).map_err(|e| e.to_string())?.dev();
        if a != b {
            return Err("on another file system than the trash; moving it there would be a copy (not done)".into());
        }
        Ok(())
    }

    fn date() -> String {
        let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let day = crate::output::date(Some(s));
        let r = s % 86400;
        format!("{day}T{:02}:{:02}:{:02}", r / 3600, r % 3600 / 60, r % 60)
    }

    pub fn send(p: &Path) -> Result<PathBuf, String> {
        let t = trash().ok_or("no trash")?;
        let base = p.file_name().ok_or("no name")?.to_string_lossy().into_owned();
        for k in 0..10_000 {
            let name = if k == 0 { base.clone() } else { format!("{base}.{k}") };
            let info = t.join("info").join(format!("{name}.trashinfo"));
            let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(&info) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let path = p.display().to_string();
            let enc: String = path.bytes().map(|b| if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                (b as char).to_string() } else { format!("%{b:02X}") }).collect();
            writeln!(f, "[Trash Info]\nPath={enc}\nDeletionDate={}", date()).map_err(|e| e.to_string())?;
            let dst = t.join("files").join(&name);
            fs::rename(p, &dst).map_err(|e| {
                let _ = fs::remove_file(&info);
                e.to_string()
            })?;
            if dst.exists() && !p.exists() {
                return Ok(dst);
            }
            return Err("moved, but not found in the trash afterwards".into());
        }
        Err("no free name in the trash".into())
    }

    pub fn restore(item: &Path, original: &Path) -> Result<(), String> {
        if original.exists() {
            return Err(format!("{} exists again", original.display()));
        }
        fs::rename(item, original).map_err(|e| e.to_string())?;
        if let (Some(t), Some(n)) = (trash(), item.file_name()) {
            let _ = fs::remove_file(t.join("info").join(format!("{}.trashinfo", n.to_string_lossy())));
        }
        Ok(())
    }
}
