//! .gitignore rules (and --exclude patterns), the parts git itself documents:
//!   blank lines and `#` comments are skipped; `\#` and `\!` escape; a leading `!` re-includes;
//!   a trailing `/` matches folders only; a `/` at the start or in the middle anchors the pattern to the
//!   .gitignore's folder; otherwise it matches a name at any depth; `**` matches any number of folders;
//!   the LAST matching rule wins. Rules from deeper .gitignore files come after (so they win).
//! Matching is on the path relative to the folder that holds the rules, with `/` separators.

use crate::glob::Glob;

#[derive(Clone)]
struct Rule {
    neg: bool,
    dir_only: bool,
    anchored: bool,
    /// the pattern split on '/', each part a glob; "**" kept as a marker
    parts: Vec<Part>,
}

#[derive(Clone)]
enum Part {
    Any,
    Glob(std::sync::Arc<Glob>),
}

#[derive(Clone, Default)]
pub struct Rules {
    /// (base: the path, relative to the walk root, of the folder holding the rules; rule)
    rules: Vec<(String, Rule)>,
}

impl Rules {
    pub fn add_lines(&mut self, base: &str, text: &str, ignore_case: bool) {
        for line in text.lines() {
            if let Some(r) = parse(line, ignore_case) {
                self.rules.push((base.to_string(), r));
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// `rel`: path relative to the walk root, `/`-separated. -> true when ignored.
    pub fn ignored(&self, rel: &str, is_dir: bool) -> bool {
        let mut verdict = false;
        for (base, r) in &self.rules {
            let sub = if base.is_empty() {
                rel
            } else if let Some(s) = rel.strip_prefix(base.as_str()).and_then(|s| s.strip_prefix('/')) {
                s
            } else {
                continue;
            };
            if r.dir_only && !is_dir {
                continue;
            }
            if matches_rule(r, sub) {
                verdict = !r.neg;
            }
        }
        verdict
    }
}

fn parse(line: &str, ignore_case: bool) -> Option<Rule> {
    let mut s = line.trim_end_matches(['\r']);
    // trailing spaces are ignored unless escaped
    let trimmed = s.trim_end();
    s = if trimmed.ends_with('\\') && s.len() > trimmed.len() { &s[..trimmed.len() + 1] } else { trimmed };
    if s.is_empty() || s.starts_with('#') {
        return None;
    }
    let mut neg = false;
    let mut p = s.to_string();
    if let Some(rest) = p.strip_prefix('!') {
        neg = true;
        p = rest.to_string();
    } else if p.starts_with("\\!") || p.starts_with("\\#") {
        p.remove(0);
    }
    let dir_only = p.ends_with('/');
    let p = p.trim_end_matches('/');
    if p.is_empty() {
        return None;
    }
    let anchored = p.starts_with('/') || p.trim_start_matches('/').contains('/');
    let p = p.trim_start_matches('/');
    let parts = p.split('/').map(|x| if x == "**" { Part::Any } else {
        Part::Glob(std::sync::Arc::new(Glob::new(x, ignore_case)))
    }).collect();
    Some(Rule { neg, dir_only, anchored, parts })
}

fn matches_rule(r: &Rule, rel: &str) -> bool {
    let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    if !r.anchored {
        // a bare name: matches the last segment at any depth
        return match r.parts.as_slice() {
            [Part::Glob(g)] => segs.last().is_some_and(|s| g.matches(s)),
            _ => false,
        };
    }
    seq(&r.parts, &segs)
}

fn seq(parts: &[Part], segs: &[&str]) -> bool {
    match parts.first() {
        None => segs.is_empty(),
        Some(Part::Any) => (0..=segs.len()).any(|k| seq(&parts[1..], &segs[k..])),
        Some(Part::Glob(g)) => !segs.is_empty() && g.matches(segs[0]) && seq(&parts[1..], &segs[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Rules {
        let mut r = Rules::default();
        r.add_lines("", text, false);
        r
    }

    #[test]
    fn names_anchors_folders_and_negation() {
        let r = rules("# comment\n\n/target\n*.log\n!keep.log\nbuild/\ndocs/**/draft*.md\n");
        assert!(r.ignored("target", true));
        assert!(!r.ignored("src/target", true), "anchored at the root only");
        assert!(r.ignored("a/b/x.log", false));
        assert!(!r.ignored("a/keep.log", false), "re-included by !");
        assert!(r.ignored("build", true));
        assert!(r.ignored("x/build", true), "folder rule at any depth");
        assert!(!r.ignored("build", false), "trailing / means folders only");
        assert!(r.ignored("docs/draft1.md", false));
        assert!(r.ignored("docs/a/b/draft2.md", false));
        assert!(!r.ignored("docs/final.md", false));
    }

    #[test]
    fn deeper_rules_are_relative_to_their_folder_and_win() {
        let mut r = rules("*.tmp\n");
        r.add_lines("sub", "!important.tmp\n/local\n", false);
        assert!(r.ignored("x.tmp", false));
        assert!(!r.ignored("sub/important.tmp", false));
        assert!(r.ignored("sub/local", true));
        assert!(!r.ignored("local", true));
    }

    #[test]
    fn escapes_and_case() {
        let r = rules("\\#notacomment\n\\!bang\n");
        assert!(r.ignored("#notacomment", false));
        assert!(r.ignored("!bang", false));
        let mut c = Rules::default();
        c.add_lines("", "*.EXE\n", true);
        assert!(c.ignored("firefox.exe", false));
    }
}
