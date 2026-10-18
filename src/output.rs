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
    /// smallest first (dust -r)
    pub reverse: bool,
    /// attributes/permissions and date on every line
    pub long: bool,
    /// one flat list of paths at --depth, biggest first, instead of a tree
    pub flat: bool,
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

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

fn long_cols(n: &Node, st: &Style) -> String {
    if st.long {
        format!("{} {}  ", crate::fastdir::attr_letters(n.attrs), date(n.mtime))
    } else {
        String::new()
    }
}

pub fn text(rep: &Report, st: &Style, out: &mut String) {
    let root = &rep.root;
    let size = |n: &Node| if st.disk { n.disk } else { n.bytes };
    let total = size(root).max(1) as f64;
    out.push_str(&format!("{:>10} {:>6}  {}  {}{}   {}, {}\n", human(size(root)), "100%",
                          bar(1.0, st.width, st.ascii), root.name, kind_mark(root),
                          plural(root.files, "file", "files"), plural(root.dirs, "folder", "folders")));
    if st.flat {
        let mut rows = Vec::new();
        flat(root, "", 1, st, &mut rows);
        rows.sort_by(|a, b| if st.reverse { a.0.cmp(&b.0) } else { b.0.cmp(&a.0) }.then_with(|| a.1.cmp(&b.1)));
        let shown = rows.len().min(st.per_level);
        for (b, p, n) in &rows[..shown] {
            out.push_str(&format!("{:>10} {:>5.1}%  {}  {}{p}\n", human(*b), *b as f64 / total * 100.0,
                                  bar(*b as f64 / total, st.width, st.ascii), long_cols(n, st)));
        }
        if rows.len() > shown {
            let b: u64 = rows[shown..].iter().map(|r| r.0).sum();
            out.push_str(&format!("{:>10} {:>5.1}%  {}  ... {} more\n", human(b), b as f64 / total * 100.0,
                                  bar(b as f64 / total, st.width, st.ascii), count((rows.len() - shown) as u64)));
        }
        return;
    }
    tree(root, "", 1, st, total, out);
}

fn flat<'a>(n: &'a Node, prefix: &str, level: usize, st: &Style, rows: &mut Vec<(u64, String, &'a Node)>) {
    for c in &n.children {
        let p = if prefix.is_empty() { c.name.clone() } else { format!("{prefix}/{}", c.name) };
        if level == st.depth || c.children.is_empty() {
            rows.push((if st.disk { c.disk } else { c.bytes }, format!("{p}{}", kind_mark(c)), c));
        } else {
            flat(c, &p, level + 1, st, rows);
        }
    }
}

fn tree(n: &Node, prefix: &str, level: usize, st: &Style, total: f64, out: &mut String) {
    if level > st.depth || n.children.is_empty() {
        return;
    }
    let size = |n: &Node| if st.disk { n.disk } else { n.bytes };
    let mut kids: Vec<&Node> = n.children.iter().collect();
    if st.reverse {
        kids.reverse();
    }
    let shown = kids.len().min(st.per_level);
    let rest = &kids[shown..];
    let (tee, ell, pipe, gap) = if st.ascii { ("|-- ", "`-- ", "|   ", "    ") } else { ("├─ ", "└─ ", "│  ", "   ") };
    for (i, c) in kids[..shown].iter().enumerate() {
        let last = i + 1 == shown && rest.is_empty();
        let frac = size(c) as f64 / total;
        let extra = if c.kind == Kind::Dir { format!("   {}", plural(c.files, "file", "files")) } else { String::new() };
        out.push_str(&format!("{:>10} {:>5.1}%  {}  {}{}{}{}{}{}\n", human(size(c)), frac * 100.0,
                              bar(frac, st.width, st.ascii), long_cols(c, st), prefix, if last { ell } else { tee },
                              c.name, kind_mark(c), extra));
        if c.kind == Kind::Dir {
            let p = format!("{prefix}{}", if last { gap } else { pipe });
            tree(c, &p, level + 1, st, total, out);
        }
    }
    if !rest.is_empty() {
        let b: u64 = rest.iter().map(|c| size(c)).sum();
        out.push_str(&format!("{:>10} {:>5.1}%  {}  {}{}... {} more\n", human(b), b as f64 / total * 100.0,
                              bar(b as f64 / total, st.width, st.ascii), prefix, ell, count(rest.len() as u64)));
    }
}

pub fn types_text(rep: &Report, n: usize, out: &mut String) {
    let total: u64 = rep.types.iter().map(|t| t.1).sum::<u64>().max(1);
    out.push_str("\nBy type:\n");
    for (ext, b, files) in rep.types.iter().take(n) {
        let e = if ext.is_empty() { "(no extension)".to_string() } else { format!(".{ext}") };
        out.push_str(&format!("{:>10} {:>5.1}%  {:>9}  {e}\n", human(*b), *b as f64 / total as f64 * 100.0,
                              plural(*files, "file", "files")));
    }
    if rep.types.len() > n {
        out.push_str(&format!("   ... {} more types\n", rep.types.len() - n));
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
    if rep.excluded > 0 {
        out.push_str(&format!("excluded by .gitignore/--exclude: {} entries (not counted)\n", count(rep.excluded)));
    }
    if rep.hidden_skipped > 0 {
        out.push_str(&format!("hidden entries skipped: {} (not counted; drop --no-hidden to count them)\n",
                              count(rep.hidden_skipped)));
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

fn kind_short(k: Kind) -> &'static str {
    match k {
        Kind::Dir => "dir ",
        Kind::File => "file",
        Kind::Link => "link",
    }
}

pub fn found_text(v: &[Found], out: &mut String) {
    for f in v {
        // find --hash: the digest goes before the path, so the path stays the last field of the line
        let h = f.sha256.as_ref().map(|h| format!("{h}  ")).unwrap_or_default();
        out.push_str(&format!("{:>10}  {}  {}  {h}{}\n", human(f.bytes), kind_short(f.kind), date(f.mtime), f.path));
    }
}

/// `--format`: {path} {name} {bytes} {size} {kind} {mtime} {date} {sha256}, \t and \n escapes.
///
/// One pass over the TEMPLATE: each placeholder is replaced by its value and the value is never looked at again.
/// 2026-10-18: the old chain of `.replace()` calls substituted inside values already inserted, so a file literally
/// named `{bytes}.txt` printed as `5.txt` under `--format {name}`, and a path holding `{size}` changed. An unknown
/// `{word}` and a lone brace are kept as they are.
pub fn format_found(fmt: &str, f: &Found) -> String {
    let name = || std::path::Path::new(&f.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = String::with_capacity(fmt.len() + f.path.len());
    let mut rest = fmt;
    while let Some(c) = rest.chars().next() {
        if c == '\\' && (rest.starts_with("\\t") || rest.starts_with("\\n")) {
            out.push(if rest.as_bytes()[1] == b't' { '\t' } else { '\n' });
            rest = &rest[2..];
            continue;
        }
        if c == '{' {
            if let Some(end) = rest.find('}') {
                let value = match &rest[1..end] {
                    "path" => Some(f.path.clone()),
                    "name" => Some(name()),
                    "bytes" => Some(f.bytes.to_string()),
                    "size" => Some(human(f.bytes)),
                    "kind" => Some(kind_name(f.kind).to_string()),
                    "mtime" => Some(f.mtime.map(|t| t.to_string()).unwrap_or_default()),
                    "date" => Some(date(f.mtime)),
                    "sha256" => Some(f.sha256.clone().unwrap_or_default()),
                    _ => None,
                };
                if let Some(v) = value {
                    out.push_str(&v);
                    rest = &rest[end + 1..];
                    continue;
                }
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
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

pub fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Dir => "dir",
        Kind::File => "file",
        Kind::Link => "link",
    }
}

pub fn node_json(n: &Node) -> Value {
    let mut v = json!({"name": n.name, "kind": kind_name(n.kind), "bytes": n.bytes, "disk_bytes": n.disk,
                       "files": n.files, "dirs": n.dirs, "mtime": n.mtime});
    if n.attrs != 0 {
        v["attrs"] = json!(crate::fastdir::attr_letters(n.attrs));
    }
    if !n.children.is_empty() {
        v["children"] = Value::Array(n.children.iter().map(node_json).collect());
    }
    v
}

pub fn skipped_json(v: &[Skipped]) -> Value {
    Value::Array(v.iter().map(|s| json!({"path": s.path, "why": s.why})).collect())
}

pub fn found_one(f: &Found) -> Value {
    let mut v = json!({"path": f.path, "kind": kind_name(f.kind), "bytes": f.bytes, "mtime": f.mtime});
    if let Some(h) = &f.sha256 {
        v["sha256"] = json!(h);
    }
    v
}

pub fn found_json(v: &[Found]) -> Value {
    Value::Array(v.iter().map(found_one).collect())
}

pub fn types_json(rep: &Report) -> Value {
    Value::Array(rep.types.iter().map(|(e, b, n)| json!({"ext": e, "bytes": b, "files": n})).collect())
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
    fn one_file_is_not_files() {
        assert_eq!(plural(1, "file", "files"), "1 file");
        assert_eq!(plural(1200, "file", "files"), "1,200 files");
        assert_eq!(plural(0, "folder", "folders"), "0 folders");
    }

    #[test]
    fn bars_fill_in_proportion() {
        assert_eq!(bar(0.5, 4, true), "##..");
        assert_eq!(bar(2.0, 3, true), "###");
        assert_eq!(bar(0.0, 2, false), "░░");
    }

    #[test]
    fn format_fields() {
        let f = Found { path: "C:/x/a.log".into(), kind: Kind::File, bytes: 2048, mtime: Some(0), sha256: None };
        assert_eq!(format_found("{name}\\t{bytes}\\t{size}\\t{date}", &f), "a.log\t2048\t2.00 KiB\t1970-01-01");
    }

    #[test]
    fn format_never_substitutes_inside_inserted_values() {
        // (no backslash in the name: on Windows it is a path separator)
        let f = Found { path: "C:/x/{bytes}{size}{path}.txt".into(), kind: Kind::File, bytes: 5, mtime: None,
                        sha256: Some("ab".into()) };
        assert_eq!(format_found("{name}|{bytes}", &f), "{bytes}{size}{path}.txt|5");
        assert_eq!(format_found("{bytes}\\t{bytes}\\n", &f), "5\t5\n");
        assert_eq!(format_found("{unknown} { {sha256}}", &f), "{unknown} { ab}");
        assert_eq!(format_found("ünï{kind}\\x", &f), "ünïfile\\x");
    }
}
