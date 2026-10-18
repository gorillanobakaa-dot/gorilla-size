//! Reading what was found, without another tool: the first or last lines, a line range, a search inside, a byte
//! comparison. Every read is capped (bytes and lines) so a model never pulls a 2 GiB log into its context by accident.
//!
//! Memory is capped too, not only the output. 2026-10-18 audit: a line was read whole before the cap was applied
//! (BufReader::lines, read_until), so a 2 GiB file with no newline - a database, a minified bundle, a log written
//! without line ends - was pulled into memory in full by --head 1 or --grep. Every line is now read through
//! `read_line_capped`, which keeps at most --max-bytes of it and streams past the rest.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// A file is binary when its first 8 KiB hold a NUL byte (git's rule).
pub fn is_binary(path: &Path) -> std::io::Result<bool> {
    Ok(sniff(path)?.is_some())
}

/// What can be said about a binary file without dumping it.
pub struct Binary {
    pub bytes: u64,
    /// from the first bytes ("magic numbers"); None when not one of the kinds below
    pub kind: Option<&'static str>,
    /// the first (up to) 16 bytes
    pub head: Vec<u8>,
}

/// None for a text file; for a binary one (a NUL in the first 8 KiB) its size, its kind when known and its first
/// 16 bytes. 2026-10-18: "binary file: not shown" told a reader nothing; the kind and the first bytes usually say
/// what the file is (an .exe renamed .log, a zip, a SQLite database) without another tool.
pub fn sniff(path: &Path) -> std::io::Result<Option<Binary>> {
    let mut f = File::open(path)?;
    let mut b = [0u8; 8192];
    let mut n = 0;
    while n < b.len() {
        let k = f.read(&mut b[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    if !b[..n].contains(&0) {
        return Ok(None);
    }
    let bytes = f.metadata().map(|m| m.len()).unwrap_or(n as u64);
    Ok(Some(Binary { bytes, kind: magic(&b[..n]), head: b[..n.min(16)].to_vec() }))
}

/// The kind of a file from its first bytes. Only formats whose signature is unambiguous at offset 0.
pub fn magic(b: &[u8]) -> Option<&'static str> {
    const KINDS: [(&[u8], &str); 11] = [
        (b"\x7fELF", "ELF"),
        (b"\x89PNG\r\n\x1a\n", "PNG"),
        (b"\xff\xd8\xff", "JPEG"),
        (b"GIF87a", "GIF"),
        (b"GIF89a", "GIF"),
        (b"PK\x03\x04", "ZIP"),
        (b"PK\x05\x06", "ZIP"),
        (b"\x1f\x8b", "GZIP"),
        (b"%PDF-", "PDF"),
        (b"SQLite format 3\0", "SQLite"),
        (b"MZ", "PE/MZ"),
    ];
    KINDS.iter().find(|(m, _)| b.starts_with(m)).map(|(_, k)| *k)
}

/// Read one line (through its '\n') into `buf`, keeping at most `cap` bytes of it; the rest of a longer line is
/// read and dropped one buffer at a time, never held. None at the end of the file, else Some(true) when the line
/// was longer than `cap`.
pub fn read_line_capped<R: BufRead>(r: &mut R, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<Option<bool>> {
    buf.clear();
    let (mut any, mut long) = (false, false);
    loop {
        let avail = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if avail.is_empty() {
            break;
        }
        any = true;
        let (chunk, end) = match avail.iter().position(|&c| c == b'\n') {
            Some(i) => (&avail[..=i], true),
            None => (avail, false),
        };
        let body = if end { chunk.len() - 1 } else { chunk.len() }; // the newline does not count against the cap
        let room = cap.saturating_sub(buf.len());
        if body > room {
            buf.extend_from_slice(&chunk[..room]);
            long = true;
        } else if !long {
            buf.extend_from_slice(chunk);
        }
        let used = chunk.len();
        r.consume(used);
        if end {
            break;
        }
    }
    Ok(any.then_some(long))
}

/// A line's text without its line end, lossily decoded.
fn text_of(buf: &[u8]) -> String {
    String::from_utf8_lossy(buf).trim_end_matches(['\n', '\r']).to_string()
}

/// The first `room` bytes of `s`, cut back to a character boundary.
fn prefix(s: &str, room: usize) -> String {
    let mut k = room.min(s.len());
    while !s.is_char_boundary(k) {
        k -= 1;
    }
    s[..k].to_string()
}

pub enum Pick {
    Head(usize),
    Tail(usize),
    Range(usize, usize),
}

/// The lines asked for, each with its 1-based number; at most `max_bytes` of text in total, and at most `max_bytes`
/// of any one line ever held in memory. The bool: the output was cut (by the total cap or by a longer line).
pub fn lines(path: &Path, pick: &Pick, max_bytes: usize) -> std::io::Result<(Vec<(usize, String)>, bool)> {
    let f = File::open(path)?;
    let mut out = Vec::new();
    let mut used = 0usize;
    let mut cut = false;
    // A line that does not fit: stop. Exception: when nothing was shown yet (one huge line), show what fits,
    // so `--head 1` of a file without line ends still shows its start.
    let mut push = |n: usize, l: String, long: bool, out: &mut Vec<(usize, String)>| -> bool {
        if used + l.len() > max_bytes || long {
            if out.is_empty() {
                let p = prefix(&l, max_bytes);
                used += p.len();
                out.push((n, p));
            }
            return false;
        }
        used += l.len();
        out.push((n, l));
        true
    };
    match *pick {
        Pick::Head(_) | Pick::Range(_, _) => {
            let (first, last) = match *pick {
                Pick::Head(k) => (1, k),
                Pick::Range(a, b) => (a.max(1), b),
                Pick::Tail(_) => unreachable!(),
            };
            let mut r = BufReader::new(f);
            let mut buf = Vec::new();
            let mut n = 0usize;
            while n < last {
                let Some(long) = read_line_capped(&mut r, &mut buf, max_bytes)? else { break };
                n += 1;
                if n < first {
                    continue;
                }
                if !push(n, text_of(&buf), long, &mut out) {
                    cut = true;
                    break;
                }
            }
        }
        Pick::Tail(k) => {
            // read backwards in blocks until k newlines are found: a 50 GiB log costs only its end
            let mut f = f;
            let len = f.seek(SeekFrom::End(0))?;
            let mut pos = len;
            let mut tail: Vec<u8> = Vec::new();
            let mut newlines = 0usize;
            while pos > 0 && newlines <= k && tail.len() < max_bytes * 2 {
                let step = pos.min(64 * 1024);
                pos -= step;
                f.seek(SeekFrom::Start(pos))?;
                let mut b = vec![0u8; step as usize];
                f.read_exact(&mut b)?;
                newlines += b.iter().filter(|&&c| c == b'\n').count();
                b.extend_from_slice(&tail);
                tail = b;
            }
            let text = String::from_utf8_lossy(&tail);
            let all: Vec<&str> = text.lines().collect();
            let start = all.len().saturating_sub(k);
            // line numbers are only known when the whole file was read back
            let base = if pos == 0 { Some(start) } else { None };
            for (j, l) in all[start..].iter().enumerate() {
                let n = base.map(|b| b + j + 1).unwrap_or(0);
                if !push(n, l.to_string(), false, &mut out) {
                    cut = true;
                    break;
                }
            }
        }
    }
    Ok((out, cut))
}

/// What a search inside one file found.
#[derive(Debug, Default)]
pub struct Grep {
    /// (1-based line number, the line shortened to `max_line` characters)
    pub hits: Vec<(usize, String)>,
    /// lines longer than the cap: only their first `cap` bytes were searched (said, never silent)
    pub long_lines: usize,
}

/// Every line through `is_hit`, at most `max_hits` hits; each line holds at most `cap` bytes in memory.
fn grep_with(path: &Path, mut is_hit: impl FnMut(&str) -> bool, max_hits: usize, max_line: usize, cap: usize)
    -> std::io::Result<Grep> {
    let mut r = BufReader::new(File::open(path)?);
    let mut buf = Vec::new();
    let (mut n, mut g) = (0usize, Grep::default());
    while let Some(long) = read_line_capped(&mut r, &mut buf, cap)? {
        n += 1;
        g.long_lines += long as usize;
        let line = text_of(&buf);
        if is_hit(&line) {
            let mut l: String = line.chars().take(max_line).collect();
            if long || line.chars().count() > max_line {
                l.push_str(" ...");
            }
            g.hits.push((n, l));
            if g.hits.len() >= max_hits {
                break;
            }
        }
    }
    Ok(g)
}

/// Lines containing `needle` (literal; `ignore_case` folds ASCII and Unicode), at most `max_hits` per file.
pub fn grep(path: &Path, needle: &str, ignore_case: bool, max_hits: usize, max_line: usize, cap: usize)
    -> std::io::Result<Grep> {
    let want = if ignore_case { needle.to_lowercase() } else { needle.to_string() };
    grep_with(path, |l| if ignore_case { l.to_lowercase().contains(&want) } else { l.contains(&want) }, max_hits,
              max_line, cap)
}

/// Lines matching the regular expression `re`, at most `max_hits` per file.
pub fn grep_re(path: &Path, re: &regex::Regex, max_hits: usize, max_line: usize, cap: usize)
    -> std::io::Result<Grep> {
    grep_with(path, |l| re.is_match(l), max_hits, max_line, cap)
}

/// true when the two files hold the same bytes (sizes first, then 1 MiB blocks).
pub fn same_bytes(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (ma, mb) = (std::fs::metadata(a)?, std::fs::metadata(b)?);
    if ma.len() != mb.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpfile(name: &str, text: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("gs-content-{}-{name}", std::process::id()));
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn head_tail_and_range_with_numbers() {
        let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let p = tmpfile("lines", &body);
        let (h, _) = lines(&p, &Pick::Head(2), 10_000).unwrap();
        assert_eq!(h, vec![(1, "line 1".into()), (2, "line 2".into())]);
        let (t, _) = lines(&p, &Pick::Tail(2), 10_000).unwrap();
        assert_eq!(t, vec![(99, "line 99".into()), (100, "line 100".into())]);
        let (r, _) = lines(&p, &Pick::Range(50, 52), 10_000).unwrap();
        assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![50, 51, 52]);
        let (c, cut) = lines(&p, &Pick::Head(100), 20).unwrap();
        assert!(cut && c.len() == 3, "the byte cap holds");
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn grep_binary_and_compare() {
        let p = tmpfile("grep", "alpha\r\nERROR one\r\nbeta\r\nerror two\r\n");
        assert_eq!(grep(&p, "ERROR", false, 10, 200, 1 << 20).unwrap().hits, vec![(2, "ERROR one".into())]);
        assert_eq!(grep(&p, "error", true, 10, 200, 1 << 20).unwrap().hits.len(), 2);
        assert_eq!(grep(&p, "error", true, 1, 200, 1 << 20).unwrap().hits.len(), 1);
        assert!(!is_binary(&p).unwrap());
        let b = std::env::temp_dir().join(format!("gs-content-{}-bin", std::process::id()));
        std::fs::write(&b, [1u8, 0, 2]).unwrap();
        assert!(is_binary(&b).unwrap());
        let q = tmpfile("grep2", "alpha\r\nERROR one\r\nbeta\r\nerror two\r\n");
        assert!(same_bytes(&p, &q).unwrap());
        assert!(!same_bytes(&p, &b).unwrap());
        for f in [p, q, b] {
            std::fs::remove_file(f).unwrap();
        }
    }

    #[test]
    fn a_long_line_is_capped_in_memory_and_the_next_line_is_still_whole() {
        let mut data = vec![b'x'; 100_000];
        data.extend_from_slice(b"\nshort\r\n");
        let mut r = BufReader::with_capacity(4096, &data[..]);
        let mut buf = Vec::new();
        assert_eq!(read_line_capped(&mut r, &mut buf, 1000).unwrap(), Some(true));
        assert_eq!(buf.len(), 1000, "never more than the cap held");
        assert_eq!(read_line_capped(&mut r, &mut buf, 1000).unwrap(), Some(false));
        assert_eq!(text_of(&buf), "short");
        assert_eq!(read_line_capped(&mut r, &mut buf, 1000).unwrap(), None);
        // exactly the cap, then the newline: not long
        let mut r = BufReader::new(&b"abcd\n"[..]);
        assert_eq!(read_line_capped(&mut r, &mut buf, 4).unwrap(), Some(false));
    }

    #[test]
    fn kinds_from_the_first_bytes() {
        assert_eq!(magic(b"MZ\x90\x00"), Some("PE/MZ"));
        assert_eq!(magic(b"\x7fELF\x02"), Some("ELF"));
        assert_eq!(magic(b"\x89PNG\r\n\x1a\n\0"), Some("PNG"));
        assert_eq!(magic(b"\xff\xd8\xff\xe0"), Some("JPEG"));
        assert_eq!(magic(b"GIF89a"), Some("GIF"));
        assert_eq!(magic(b"PK\x03\x04"), Some("ZIP"));
        assert_eq!(magic(b"\x1f\x8b\x08"), Some("GZIP"));
        assert_eq!(magic(b"%PDF-1.7"), Some("PDF"));
        assert_eq!(magic(b"SQLite format 3\0"), Some("SQLite"));
        assert_eq!(magic(b"\0\x01"), None);
    }
}
