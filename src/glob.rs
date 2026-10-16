//! Name patterns: `*` (any run), `?` (one character), `[abc]` / `[a-z]` (one of). Case-insensitive on Windows and
//! macOS (their file systems are), case-sensitive elsewhere unless asked. Matched against the NAME, not the path.

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
