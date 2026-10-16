//! Reading what was found, without another tool: the first or last lines, a line range, a search inside, a byte
//! comparison. Every read is capped (bytes and lines) so a model never pulls a 2 GiB log into its context by accident.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// A file is binary when its first 8 KiB hold a NUL byte (git's rule).
pub fn is_binary(path: &Path) -> std::io::Result<bool> {
    let mut f = File::open(path)?;
    let mut b = [0u8; 8192];
    let n = f.read(&mut b)?;
    Ok(b[..n].contains(&0))
}

pub enum Pick {
    Head(usize),
    Tail(usize),
    Range(usize, usize),
}

/// The lines asked for, each with its 1-based number; at most `max_bytes` of text in total.
pub fn lines(path: &Path, pick: &Pick, max_bytes: usize) -> std::io::Result<(Vec<(usize, String)>, bool)> {
    let f = File::open(path)?;
    let mut out = Vec::new();
    let mut used = 0usize;
    let mut cut = false;
    let mut push = |n: usize, l: String, out: &mut Vec<(usize, String)>| -> bool {
        if used + l.len() > max_bytes {
            return false;
        }
        used += l.len();
        out.push((n, l));
        true
    };
    match *pick {
        Pick::Head(k) => {
            for (i, l) in BufReader::new(f).lines().enumerate().take(k) {
                let l = l.unwrap_or_else(|_| "<not text>".into());
                if !push(i + 1, l, &mut out) {
                    cut = true;
                    break;
                }
            }
        }
        Pick::Range(a, b) => {
            let a = a.max(1);
            for (i, l) in BufReader::new(f).lines().enumerate().skip(a - 1).take((b + 1).saturating_sub(a)) {
                let l = l.unwrap_or_else(|_| "<not text>".into());
                if !push(i + 1, l, &mut out) {
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
                if !push(n, l.to_string(), &mut out) {
                    cut = true;
                    break;
                }
            }
        }
    }
    Ok((out, cut))
}

/// Lines containing `needle` (literal; `ignore_case` folds ASCII and Unicode), at most `max_hits` per file.
pub fn grep(path: &Path, needle: &str, ignore_case: bool, max_hits: usize, max_line: usize)
    -> std::io::Result<Vec<(usize, String)>> {
    let f = File::open(path)?;
    let want = if ignore_case { needle.to_lowercase() } else { needle.to_string() };
    let mut out = Vec::new();
    let mut r = BufReader::new(f);
    let mut buf = Vec::new();
    let mut n = 0usize;
    loop {
        buf.clear();
        if r.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        n += 1;
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        let hit = if ignore_case { line.to_lowercase().contains(&want) } else { line.contains(&want) };
        if hit {
            let mut l: String = line.chars().take(max_line).collect();
            if line.chars().count() > max_line {
                l.push_str(" ...");
            }
            out.push((n, l));
            if out.len() >= max_hits {
                break;
            }
        }
    }
    Ok(out)
}

/// Lines matching the regular expression `re`, at most `max_hits` per file.
pub fn grep_re(path: &Path, re: &regex::Regex, max_hits: usize, max_line: usize) -> std::io::Result<Vec<(usize, String)>> {
    let mut r = BufReader::new(File::open(path)?);
    let mut buf = Vec::new();
    let (mut n, mut out) = (0usize, Vec::new());
    loop {
        buf.clear();
        if r.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        n += 1;
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if re.is_match(line) {
            let mut l: String = line.chars().take(max_line).collect();
            if line.chars().count() > max_line {
                l.push_str(" ...");
            }
            out.push((n, l));
            if out.len() >= max_hits {
                break;
            }
        }
    }
    Ok(out)
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
        assert_eq!(grep(&p, "ERROR", false, 10, 200).unwrap(), vec![(2, "ERROR one".into())]);
        assert_eq!(grep(&p, "error", true, 10, 200).unwrap().len(), 2);
        assert_eq!(grep(&p, "error", true, 1, 200).unwrap().len(), 1);
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
}
