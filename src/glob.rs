//! Name patterns: `*` (any run), `?` (one character), `[abc]` / `[a-z]` (one of). Case-insensitive on Windows and
//! macOS (their file systems are), case-sensitive elsewhere unless asked. Matched against the NAME, not the path.

use std::path::{Component, Path, PathBuf};

pub struct Glob {
    pat: Vec<char>,
    fold: bool,
}

impl Glob {
    pub fn new(pattern: &str, ignore_case: bool) -> Glob {
        let fold = ignore_case;
        let p = if fold { pattern.to_lowercase() } else { pattern.to_string() };
        Glob { pat: p.chars().collect(), fold }
    }

    pub fn matches(&self, name: &str) -> bool {
        let n: Vec<char> = if self.fold { name.to_lowercase().chars().collect() } else { name.chars().collect() };
        matches(&self.pat, &n)
    }
}

/// Iterative matcher with single-star backtracking: linear in practice, no recursion blow-up on `*a*a*a*`.
fn matches(p: &[char], n: &[char]) -> bool {
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ni));
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ni += 1;
                    continue;
                }
                '[' => {
                    if let Some((ok, len)) = class(&p[pi..], n[ni]) {
                        if ok {
                            pi += len;
                            ni += 1;
                            continue;
                        }
                    } else if n[ni] == '[' {
                        pi += 1;
                        ni += 1;
                        continue;
                    }
                }
                c if c == n[ni] => {
                    pi += 1;
                    ni += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, sn)) => {
                pi = sp + 1;
                ni = sn + 1;
                star = Some((sp, sn + 1));
            }
            None => return false,
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// `[...]` at the start of `p`: Some((matched, length of the class)) or None if it is not a closed class.
fn class(p: &[char], c: char) -> Option<(bool, usize)> {
    let end = p.iter().skip(1).position(|&x| x == ']')? + 1;
    let body = &p[1..end];
    let (neg, body) = match body.first() {
        Some('!') | Some('^') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut hit = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == '-' {
            if body[i] <= c && c <= body[i + 2] {
                hit = true;
            }
            i += 3;
        } else {
            if body[i] == c {
                hit = true;
            }
            i += 1;
        }
    }
    Some((hit != neg, end + 1))
}

// ------------------------------------------------------------------------------------------- paths with patterns

/// `*` or `?` in a path component. `[` alone does not make a pattern: "photo [1].png" is a common real name.
pub fn is_pattern(c: &str) -> bool {
    c.contains('*') || c.contains('?')
}

/// A path the action commands must expand: some component holds `*` or `?` and the path does not exist as written
/// (Linux allows `*` in a real name; that name wins).
pub fn path_is_pattern(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_err()
        && p.components().any(|c| matches!(c, Component::Normal(s) if is_pattern(&s.to_string_lossy())))
}

#[derive(Debug, Default)]
pub struct Expansion {
    /// the FILES matched (links and junctions count as files: never entered), sorted
    pub files: Vec<PathBuf>,
    /// folders a pattern named in its last place: never matched (a pattern recycles files, not trees)
    pub folders_skipped: Vec<PathBuf>,
    /// the path up to the first pattern component
    pub base: PathBuf,
}

/// Expand a path pattern. Each component is a name pattern for the entries of ONE folder level: `dir\*.png` is the
/// PNG files directly in dir, never below. `**` is any depth of folders (zero included): `dir\**\*.png` is every PNG
/// under dir; a trailing `**` is every file under it. A component that exists as written is taken as written
/// (`dir [1]\*.png`). Links and junctions are never entered. The last component matches files only; a folder it
/// names is reported in `folders_skipped`, not returned.
pub fn expand(p: &Path) -> Expansion {
    let mut base = PathBuf::new();
    let mut rest: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) if !rest.is_empty() || is_pattern(&s.to_string_lossy()) => {
                rest.push(s.to_string_lossy().into_owned())
            }
            other => base.push(other.as_os_str()),
        }
    }
    let mut x = Expansion { base: base.clone(), ..Default::default() };
    walk(&base, &rest, &mut x);
    x.files.sort();
    x.files.dedup();
    x.folders_skipped.sort();
    x.folders_skipped.dedup();
    x
}

fn real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.is_dir() && !m.file_type().is_symlink() && !crate::os::is_reparse_dir(&m))
        .unwrap_or(false)
}

fn subdirs(cur: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(cur).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| real_dir(p))
        .collect()).unwrap_or_default();
    v.sort();
    v
}

fn all_files(cur: &Path, x: &mut Expansion) {
    if let Ok(rd) = std::fs::read_dir(cur) {
        for e in rd.flatten() {
            let p = e.path();
            if real_dir(&p) { all_files(&p, x) } else { x.files.push(p) }
        }
    }
}

fn walk(cur: &Path, rest: &[String], x: &mut Expansion) {
    let Some((c, more)) = rest.split_first() else {
        match std::fs::symlink_metadata(cur) {
            Ok(_) if real_dir(cur) => x.folders_skipped.push(cur.to_path_buf()),
            Ok(_) => x.files.push(cur.to_path_buf()),
            Err(_) => {}
        }
        return;
    };
    if c == "**" {
        if more.is_empty() {
            return all_files(cur, x);
        }
        walk(cur, more, x);
        for d in subdirs(cur) {
            walk(&d, rest, x);
        }
        return;
    }
    let literal = cur.join(c);
    if !is_pattern(c) || std::fs::symlink_metadata(&literal).is_ok() {
        if more.is_empty() || real_dir(&literal) {
            walk(&literal, more, x);
        }
        return;
    }
    let g = Glob::new(c, cfg!(any(windows, target_os = "macos")));
    let Ok(rd) = std::fs::read_dir(cur) else { return };
    let mut hits: Vec<PathBuf> = rd.flatten().filter(|e| g.matches(&e.file_name().to_string_lossy()))
        .map(|e| e.path()).collect();
    hits.sort();
    for h in hits {
        if more.is_empty() || real_dir(&h) {
            walk(&h, more, x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_questions_and_classes() {
        let g = |p: &str, n: &str| Glob::new(p, false).matches(n);
        assert!(g("*.patch", "001-fix.patch"));
        assert!(!g("*.patch", "001-fix.patch.orig"));
        assert!(g("157.0-*", "157.0-clean"));
        assert!(g("file?.txt", "file1.txt"));
        assert!(!g("file?.txt", "file10.txt"));
        assert!(g("[0-9]*", "7zip"));
        assert!(!g("[!0-9]*", "7zip"));
        assert!(g("*a*a*a*b", "aaaaaaaaaaaaaaaaaaaab"));
        assert!(!g("*a*a*a*b", "aaaaaaaaaaaaaaaaaaaaa"));
        assert!(g("*", ""));
    }

    #[test]
    fn case_folding_is_a_choice() {
        assert!(Glob::new("*.EXE", true).matches("firefox.exe"));
        assert!(!Glob::new("*.EXE", false).matches("firefox.exe"));
    }
}
