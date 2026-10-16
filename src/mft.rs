//! --mft: read a whole NTFS volume from its master file table instead of walking folders (the way WizTree does it).
//!
//! Windows only, administrator only (the volume is opened raw, read-only). Every file record is read in large
//! sequential chunks straight from the disk; sizes come from each file's unnamed $DATA attribute, parents and names
//! from $FILE_NAME. A file with several hard links has one record, so it is counted once by construction.
//!
//! What it reads and why it is safe: the volume handle is opened with GENERIC_READ only; nothing is written, locked
//! or dismounted. The MFT's own location comes from the boot sector, its extents from its own record 0.
//!
//! Limits, said plainly: an $ATTRIBUTE_LIST (very fragmented or very many-named files) is followed only for the
//! records it names inside the MFT already read, which covers the size; alternate data streams are not counted
//! (Explorer does not count them either); only paths under the PATH given are reported.

use crate::walk::Report;
use std::path::Path;

#[cfg(not(windows))]
pub fn run(_path: &Path, _depth: usize, _top: usize) -> Result<Report, String> {
    Err("the master file table is an NTFS (Windows) structure; on this system the normal walk is used".into())
}

#[cfg(windows)]
pub fn run(path: &Path, depth: usize, top: usize) -> Result<Report, String> {
    imp::run(path, depth, top)
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::path::{Component, Path, Prefix};

    use crate::os::win::{self, CloseHandle, CreateFileW, Handle, INVALID};
    use crate::walk::{Kind, Node, Report};

    #[link(name = "kernel32")]
    extern "system" {
        fn ReadFile(h: Handle, buf: *mut c_void, n: u32, read: *mut u32, ov: *mut c_void) -> i32;
        fn SetFilePointerEx(h: Handle, dist: i64, newpos: *mut i64, method: u32) -> i32;
        fn GetLastError() -> u32;
    }

    const ROOT_RECORD: u64 = 5;

    struct Vol {
        h: Handle,
    }
    impl Drop for Vol {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.h) };
        }
    }
    impl Vol {
        fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<(), String> {
            unsafe {
                if SetFilePointerEx(self.h, off as i64, std::ptr::null_mut(), 0) == 0 {
                    return Err(format!("seek failed (error {})", GetLastError()));
                }
                let mut done = 0usize;
                while done < buf.len() {
                    let mut n = 0u32;
                    let want = (buf.len() - done).min(64 << 20) as u32;
                    if ReadFile(self.h, buf[done..].as_mut_ptr() as *mut c_void, want, &mut n, std::ptr::null_mut()) == 0 {
                        return Err(format!("read failed (error {})", GetLastError()));
                    }
                    if n == 0 {
                        return Err("unexpected end of volume".into());
                    }
                    done += n as usize;
                }
            }
            Ok(())
        }
    }

    fn u16le(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    }
    fn u32le(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }
    fn u64le(b: &[u8], o: usize) -> u64 {
        u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
    }

    /// Undo the update sequence ("fixup"): the last two bytes of every sector were swapped out for a check value.
    /// A record whose check value does not match was torn mid-write; it is skipped.
    fn fixup(rec: &mut [u8], sector: usize) -> bool {
        let usa_off = u16le(rec, 4) as usize;
        let usa_n = u16le(rec, 6) as usize;
        if usa_n < 2 || usa_off + usa_n * 2 > rec.len() {
            return false;
        }
        let check = [rec[usa_off], rec[usa_off + 1]];
        for i in 1..usa_n {
            let end = i * sector;
            if end > rec.len() {
                break;
            }
            if rec[end - 2..end] != check {
                return false;
            }
            rec[end - 2] = rec[usa_off + i * 2];
            rec[end - 1] = rec[usa_off + i * 2 + 1];
        }
        true
    }

    /// Data runs -> [(start cluster, length in clusters)]
    fn runs(b: &[u8]) -> Vec<(i64, u64)> {
        let mut out = Vec::new();
        let (mut i, mut lcn) = (0usize, 0i64);
        while i < b.len() && b[i] != 0 {
            let h = b[i];
            let (ln, off) = ((h & 0xF) as usize, (h >> 4) as usize);
            i += 1;
            if i + ln + off > b.len() {
                break;
            }
            let mut len = 0u64;
            for k in 0..ln {
                len |= (b[i + k] as u64) << (8 * k);
            }
            i += ln;
            if off > 0 {
                let mut d = 0i64;
                for k in 0..off {
                    d |= (b[i + k] as i64) << (8 * k);
                }
                if b[i + off - 1] & 0x80 != 0 {
                    d -= 1i64 << (8 * off);
                }
                lcn += d;
                out.push((lcn, len));
            } else {
                out.push((-1, len)); // sparse
            }
            i += off;
        }
        out
    }

    #[derive(Default, Clone)]
    struct Rec {
        parent: u64,
        name: String,
        name_space: u8,
        dir: bool,
        bytes: u64,
        disk: u64,
        mtime: Option<u64>,
        base: u64,
        used: bool,
        link: bool,
    }

    fn filetime_to_unix(ft: u64) -> Option<u64> {
        (ft / 10_000_000).checked_sub(11_644_473_600)
    }

    /// One file record -> what we need from it. `rec` is fixed up already.
    fn parse(rec: &[u8], cluster: u64) -> Option<Rec> {
        if &rec[0..4] != b"FILE" {
            return None;
        }
        let flags = u16le(rec, 22);
        let mut r = Rec { used: flags & 1 != 0, dir: flags & 2 != 0, base: u64le(rec, 32) & 0xFFFF_FFFF_FFFF,
                          ..Default::default() };
        if !r.used {
            return Some(r);
        }
        let mut a = u16le(rec, 20) as usize;
        let mut best_ns = 255u8;
        while a + 16 <= rec.len() {
            let typ = u32le(rec, a);
            if typ == 0xFFFF_FFFF {
                break;
            }
            let len = u32le(rec, a + 4) as usize;
            if len == 0 || a + len > rec.len() {
                break;
            }
            let nonres = rec[a + 8] != 0;
            let name_len = rec[a + 9];
            match typ {
                0x10 if !nonres => {
                    // $STANDARD_INFORMATION: modification time, reparse flag
                    let vo = a + u16le(rec, a + 20) as usize;
                    if vo + 0x24 <= rec.len() {
                        r.mtime = filetime_to_unix(u64le(rec, vo + 8));
                        r.link = u32le(rec, vo + 0x20) & 0x400 != 0 && r.dir;
                    }
                }
                0x30 if !nonres => {
                    // $FILE_NAME: prefer the Win32 name over the DOS 8.3 one
                    let vo = a + u16le(rec, a + 20) as usize;
                    if vo + 66 <= rec.len() {
                        let n = rec[vo + 64] as usize;
                        let ns = rec[vo + 65];
                        let rank = match ns { 1 | 3 => 0, 0 => 1, _ => 2 };
                        if rank < best_ns && vo + 66 + n * 2 <= rec.len() {
                            best_ns = rank;
                            r.parent = u64le(rec, vo) & 0xFFFF_FFFF_FFFF;
                            let w: Vec<u16> = (0..n).map(|k| u16le(rec, vo + 66 + k * 2)).collect();
                            r.name = String::from_utf16_lossy(&w);
                            r.name_space = ns;
                        }
                    }
                }
                0x80 if name_len == 0 => {
                    // the unnamed $DATA stream: the file's size
                    if nonres {
                        if u64le(rec, a + 16) == 0 {
                            r.bytes = u64le(rec, a + 48);
                            r.disk = if u16le(rec, a + 12) != 0 && len >= 72 { u64le(rec, a + 64) } else { u64le(rec, a + 40) };
                        }
                    } else {
                        r.bytes = u32le(rec, a + 16) as u64;
                        r.disk = 0; // resident: lives inside the record itself
                    }
                }
                _ => {}
            }
            a += len;
        }
        let _ = cluster;
        Some(r)
    }

    fn drive_letter(p: &Path) -> Option<char> {
        match p.components().next()? {
            Component::Prefix(px) => match px.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some((d as char).to_ascii_uppercase()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn run(path: &Path, depth: usize, top: usize) -> Result<Report, String> {
        if !win::is_admin() {
            return Err("needs administrator rights (the volume is read raw). Run it from an administrator \
                        prompt, or let Fieldkit ask once: fieldkit size --mft <path> (one UAC click)".into());
        }
        let letter = drive_letter(path).ok_or("give a path on a lettered drive, like C:\\Users")?;
        let dev: Vec<u16> = format!(r"\\.\{letter}:").encode_utf16().chain(Some(0)).collect();
        const GENERIC_READ: u32 = 0x8000_0000;
        let h = unsafe { CreateFileW(dev.as_ptr(), GENERIC_READ, 3, std::ptr::null_mut(), 3, 0, std::ptr::null_mut()) };
        if h == INVALID {
            return Err(format!("could not open {letter}: for reading (error {})", unsafe { GetLastError() }));
        }
        let vol = Vol { h };
        let mut boot = vec![0u8; 512];
        vol.read_at(0, &mut boot)?;
        if &boot[3..11] != b"NTFS    " {
            return Err(format!("{letter}: is not NTFS (only NTFS has a master file table); use the normal walk"));
        }
        let sector = u16le(&boot, 11) as u64;
        let cluster = sector * boot[13] as u64;
        let mft_lcn = u64le(&boot, 48);
        let crec = boot[64] as i8;
        let rec_size = if crec < 0 { 1u64 << (-crec) } else { crec as u64 * cluster } as usize;

        // record 0 is $MFT itself: its $DATA runs say where every part of the table is
        let mut r0 = vec![0u8; rec_size];
        vol.read_at(mft_lcn * cluster, &mut r0)?;
        if !fixup(&mut r0, sector as usize) {
            return Err("the master file table's own record is damaged".into());
        }
        let extents = mft_runs(&r0).ok_or("could not read where the master file table lies")?;

        let mut recs: Vec<Rec> = Vec::new();
        let chunk = (32usize << 20) / rec_size * rec_size;
        let mut buf = vec![0u8; chunk];
        for (lcn, len) in extents {
            if lcn < 0 {
                continue;
            }
            let (mut off, end) = (lcn as u64 * cluster, (lcn as u64 + len) * cluster);
            while off < end {
                let n = ((end - off) as usize).min(chunk);
                vol.read_at(off, &mut buf[..n])?;
                for rb in buf[..n].chunks_exact_mut(rec_size) {
                    let r = if fixup(rb, sector as usize) { parse(rb, cluster) } else { None };
                    recs.push(r.unwrap_or_default());
                }
                off += n as u64;
            }
        }
        // extension records (named by a base record) add their sizes to the base
        for i in 0..recs.len() {
            let b = recs[i].base;
            if recs[i].used && b != 0 && (b as usize) < recs.len() {
                let (bytes, disk) = (recs[i].bytes, recs[i].disk);
                let base = &mut recs[b as usize];
                if base.bytes == 0 {
                    base.bytes = bytes;
                    base.disk = disk;
                }
                recs[i].used = false;
            }
        }
        build(&recs, path, letter, depth, top)
    }

    fn mft_runs(r0: &[u8]) -> Option<Vec<(i64, u64)>> {
        let mut a = u16le(r0, 20) as usize;
        while a + 16 <= r0.len() {
            let typ = u32le(r0, a);
            let len = u32le(r0, a + 4) as usize;
            if typ == 0xFFFF_FFFF || len == 0 || a + len > r0.len() {
                return None;
            }
            if typ == 0x80 && r0[a + 8] != 0 && r0[a + 9] == 0 {
                let ro = a + u16le(r0, a + 32) as usize;
                return Some(runs(&r0[ro..a + len]));
            }
            a += len;
        }
        None
    }

    /// The flat table -> the same tree the walk builds, for the folder asked for.
    fn build(recs: &[Rec], path: &Path, letter: char, depth: usize, top: usize) -> Result<Report, String> {
        let n = recs.len();
        let ok = |i: usize| recs[i].used && !recs[i].name.is_empty() || i == ROOT_RECORD as usize;
        // children lists
        let mut kids: Vec<Vec<u32>> = vec![Vec::new(); n];
        for i in 0..n {
            if i != ROOT_RECORD as usize && ok(i) && recs[i].base == 0 {
                let p = recs[i].parent as usize;
                if p < n && p != i {
                    kids[p].push(i as u32);
                }
            }
        }
        // find the record of `path`, by name, from the root
        let mut cur = ROOT_RECORD as usize;
        for c in path.components() {
            if let Component::Normal(s) = c {
                let want = s.to_string_lossy().to_lowercase();
                cur = *kids[cur].iter().find(|&&k| recs[k as usize].name.to_lowercase() == want)
                    .ok_or(format!("{} is not on {letter}: as read from the master file table", path.display()))?
                    as usize;
            }
        }
        // sizes bottom-up, iteratively (a deep tree must not overflow the stack)
        let mut tot = vec![(0u64, 0u64, 0u64, 0u64); n]; // bytes, disk, files, dirs
        let mut order = Vec::new();
        let mut stack = vec![cur];
        let mut seen = vec![false; n];
        while let Some(i) = stack.pop() {
            if seen[i] {
                continue;
            }
            seen[i] = true;
            order.push(i);
            if recs[i].dir && !recs[i].link || i == cur {
                stack.extend(kids[i].iter().map(|&k| k as usize));
            }
        }
        let mut links = Vec::new();
        let mut topv: Vec<(u64, String)> = Vec::new();
        for &i in order.iter().rev() {
            let r = &recs[i];
            let mut t = if r.dir { (0, 0, 0, 0) } else { (r.bytes, r.disk, 1, 0) };
            if r.dir && !r.link || i == cur {
                for &k in &kids[i] {
                    let (b, d, f, s) = tot[k as usize];
                    t.0 += b;
                    t.1 += d;
                    t.2 += f;
                    t.3 += s + recs[k as usize].dir as u64;
                }
            }
            tot[i] = t;
            if r.link && i != cur {
                links.push(crate::walk::Skipped { path: r.name.clone(), why: "junction or link (MFT)".into() });
            }
            if !r.dir && top > 0 {
                topv.push((r.bytes, r.name.clone()));
            }
        }
        topv.sort_by(|a, b| b.0.cmp(&a.0));
        topv.truncate(top);
        fn node(i: usize, level: usize, depth: usize, recs: &[Rec], kids: &[Vec<u32>],
                tot: &[(u64, u64, u64, u64)], name: String) -> Node {
            let r = &recs[i];
            let (b, d, f, s) = tot[i];
            let kind = if r.link { Kind::Link } else if r.dir || level == 0 { Kind::Dir } else { Kind::File };
            let mut children = Vec::new();
            if level < depth && kind == Kind::Dir {
                children = kids[i].iter().map(|&k| node(k as usize, level + 1, depth, recs, kids, tot,
                                                        recs[k as usize].name.clone())).collect();
                children.sort_by(|a: &Node, b: &Node| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
            }
            Node { name, kind, bytes: b, disk: d, files: f, dirs: s, mtime: r.mtime, children }
        }
        let root = node(cur, 0, depth, recs, &kids, &tot, path.display().to_string());
        Ok(Report { root, top_files: topv, links, errors: vec![], other_fs: vec![], found: vec![],
                    hardlink_repeats: 0, hardlink_bytes: 0 })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn data_runs_decode_with_negative_offsets_and_sparse_runs() {
            // 0x21: 1-byte length, 2-byte offset; then 0x01: sparse; then 0x11 with a negative offset (-2)
            let b = [0x21, 0x10, 0x00, 0x01, 0x01, 0x05, 0x11, 0x03, 0xFE, 0x00];
            assert_eq!(runs(&b), vec![(256, 16), (-1, 5), (254, 3)]);
        }

        #[test]
        fn a_torn_record_is_refused() {
            let mut rec = vec![0u8; 1024];
            rec[4] = 48; // update sequence at 48
            rec[6] = 3; // check value + 2 sector entries
            rec[48] = 0xAB;
            rec[49] = 0xCD;
            rec[510] = 0xAB;
            rec[511] = 0xCD;
            rec[1022] = 0xAB;
            rec[1023] = 0xCD;
            rec[50] = 1;
            rec[52] = 2;
            assert!(fixup(&mut rec.clone(), 512));
            let mut torn = rec.clone();
            torn[1023] = 0;
            assert!(!fixup(&mut torn, 512));
            let mut fixed = rec;
            fixup(&mut fixed, 512);
            assert_eq!(fixed[510], 1);
            assert_eq!(fixed[1022], 2);
        }

        #[test]
        fn filetimes_become_unix_seconds() {
            assert_eq!(filetime_to_unix(116_444_736_000_000_000), Some(0));
            assert_eq!(filetime_to_unix(0), None);
        }
    }
}
