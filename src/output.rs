//! Reports for a person (bars and a tree, like dust and erdtree) and for a program (JSON, everything the walk kept).

use serde_json::{json, Value};

use crate::units::{count, human};
use crate::walk::{Found, Kind, Node, Report, Skipped};

pub struct Style {
    pub ascii: bool,
    pub per_level: usize,
    pub depth: usize,
    pub disk: bool,
    pub width: usize,
}

fn bar(frac: f64, width: usize, ascii: bool) -> String {
    let full = (frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    let (on, off) = if ascii { ('#', '.') } else { ('█', '░') };
    std::iter::repeat(on).take(full).chain(std::iter::repeat(off).take(width - full)).collect()
}

fn kind_mark(n: &Node) -> &'static str {
    match n.kind {
        Kind::Dir => "/",
        Kind::Link => " -> (link, not followed)",
        Kind::File => "",
    }
}

pub fn text(rep: &Report, st: &Style, out: &mut String) {
    let root = &rep.root;
    let total = root.bytes.max(1) as f64;
    let size = |n: &Node| if st.disk { n.disk } else { n.bytes };
    out.push_str(&format!("{:>10} {:>6}  {}  {}{}   {} files, {} folders\n", human(size(root)), "100%",
                          bar(1.0, st.width, st.ascii), root.name, kind_mark(root), count(root.files),
                          count(root.dirs)));
    tree(root, "", 1, st, total, out);
}

fn tree(n: &Node, prefix: &str, level: usize, st: &Style, total: f64, out: &mut String) {
    if level > st.depth || n.children.is_empty() {
        return;
    }
    let size = |n: &Node| if st.disk { n.disk } else { n.bytes };
    let shown = n.children.len().min(st.per_level);
    let rest = &n.children[shown..];
    let (tee, ell, pipe, gap) = if st.ascii { ("|-- ", "`-- ", "|   ", "    ") } else { ("├─ ", "└─ ", "│  ", "   ") };
    for (i, c) in n.children[..shown].iter().enumerate() {
        let last = i + 1 == shown && rest.is_empty();
        let frac = size(c) as f64 / total;
        let extra = if c.kind == Kind::Dir { format!("   {} files", count(c.files)) } else { String::new() };
        out.push_str(&format!("{:>10} {:>5.1}%  {}  {}{}{}{}{}\n", human(size(c)), frac * 100.0,
                              bar(frac, st.width, st.ascii), prefix, if last { ell } else { tee }, c.name,
                              kind_mark(c), extra));
        if c.kind == Kind::Dir {
            let p = format!("{prefix}{}", if last { gap } else { pipe });
            tree(c, &p, level + 1, st, total, out);
        }
    }
    if !rest.is_empty() {
        let b: u64 = rest.iter().map(size).sum();
        out.push_str(&format!("{:>10} {:>5.1}%  {}  {}{}... {} more\n", human(b), b as f64 / total * 100.0,
                              bar(b as f64 / total, st.width, st.ascii), prefix, ell, count(rest.len() as u64)));
    }
}

pub fn notes(rep: &Report, disk_measured: bool, hardlinks_checked: bool, out: &mut String) {
    if !rep.top_files.is_empty() {
        out.push_str("\nLargest files:\n");
        for (b, p) in &rep.top_files {
            out.push_str(&format!("{:>10}  {p}\n", human(*b)));
        }
    }
    out.push('\n');
    out.push_str(&format!("links not followed (junctions, symlinks, mount points): {}{}\n", count(rep.links.len() as u64),
                          if rep.links.is_empty() { "" } else { "  (--list-links to see them)" }));
    if !rep.other_fs.is_empty() {
        out.push_str(&format!("other file systems not entered: {} (--cross-filesystems to enter them)\n",
                              count(rep.other_fs.len() as u64)));
    }
    out.push_str(&format!("unreadable: {}{}\n", count(rep.errors.len() as u64),
                          if rep.errors.is_empty() { "" } else { "  (--list-errors to see them)" }));
    if hardlinks_checked {
        out.push_str(&format!("hard links: counted once ({} repeat names, {} not counted twice)\n",
                              count(rep.hardlink_repeats), human(rep.hardlink_bytes)));
    } else {
        out.push_str("hard links: NOT checked (Windows lists them as separate files; --links counts them once)\n");
    }
    if !disk_measured {
        out.push_str("size: apparent (what the files hold); --disk for the space they take on the disk\n");
    }
}

pub fn skipped_list(title: &str, v: &[Skipped], out: &mut String) {
    out.push_str(&format!("\n{title}:\n"));
    for s in v {
        out.push_str(&format!("  {}  ({})\n", s.path, s.why));
    }
}

pub fn found_text(v: &[Found], out: &mut String) {
    for f in v {
        let k = match f.kind {
            Kind::Dir => "dir ",
            Kind::File => "file",
            Kind::Link => "link",
        };
        out.push_str(&format!("{:>10}  {k}  {}  {}\n", human(f.bytes), date(f.mtime), f.path));
    }
}

/// seconds since 1970 -> "2026-10-16" (UTC); "-" when unknown
pub fn date(t: Option<u64>) -> String {
    let Some(s) = t else { return "-         ".into() };
    let days = (s / 86400) as i64;
    // civil-from-days (H. Hinnant), exact for the proleptic Gregorian calendar
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Dir => "dir",
        Kind::File => "file",
        Kind::Link => "link",
    }
}

pub fn node_json(n: &Node) -> Value {
    let mut v = json!({"name": n.name, "kind": kind_name(n.kind), "bytes": n.bytes, "disk_bytes": n.disk,
                       "files": n.files, "dirs": n.dirs, "mtime": n.mtime});
    if !n.children.is_empty() {
        v["children"] = Value::Array(n.children.iter().map(node_json).collect());
    }
    v
}

pub fn skipped_json(v: &[Skipped]) -> Value {
    Value::Array(v.iter().map(|s| json!({"path": s.path, "why": s.why})).collect())
}

pub fn found_json(v: &[Found]) -> Value {
    Value::Array(v.iter().map(|f| json!({"path": f.path, "kind": kind_name(f.kind), "bytes": f.bytes,
                                         "mtime": f.mtime})).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_exact() {
        assert_eq!(date(Some(0)), "1970-01-01");
        assert_eq!(date(Some(1_792_108_800)), "2026-10-16");
        assert_eq!(date(Some(951_782_400)), "2000-02-29");
    }

    #[test]
    fn bars_fill_in_proportion() {
        assert_eq!(bar(0.5, 4, true), "##..");
        assert_eq!(bar(2.0, 3, true), "###");
        assert_eq!(bar(0.0, 2, false), "░░");
    }
}
