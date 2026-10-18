//! Two JSON reports of the same place, made at different times: what grew, what shrank, what is new, what went.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::units::human;

fn flatten(v: &Value, prefix: &str, out: &mut BTreeMap<String, u64>) {
    let name = v["name"].as_str().unwrap_or("?");
    let path = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
    out.insert(path.clone(), v["bytes"].as_u64().unwrap_or(0));
    if let Some(kids) = v["children"].as_array() {
        for k in kids {
            flatten(k, &path, out);
        }
    }
}

/// The `roots` of a gorilla-size JSON report.
fn load_roots(path: &str) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    // Windows PowerShell 5.1 writes UTF-8 with a byte-order mark (Set-Content -Encoding utf8, Out-File); JSON parsers
    // reject it. 2026-10-16: a report saved that way was refused as "not a gorilla-size JSON report".
    let text = text.trim_start_matches('\u{feff}');
    let v: Value = serde_json::from_str(text).map_err(|e| format!("{path}: not a gorilla-size JSON report: {e}"))?;
    let roots = v["roots"].as_array().ok_or(format!("{path}: no \"roots\" (make it with gorilla-size --json)"))?;
    Ok(roots.clone())
}

fn load(path: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut m = BTreeMap::new();
    for r in load_roots(path)? {
        flatten(&r["tree"], "", &mut m);
    }
    Ok(m)
}

pub struct Change {
    pub path: String,
    pub before: Option<u64>,
    pub after: Option<u64>,
}

impl Change {
    pub fn delta(&self) -> i128 {
        self.after.unwrap_or(0) as i128 - self.before.unwrap_or(0) as i128
    }
}

pub fn diff(old: &str, new: &str) -> Result<Vec<Change>, String> {
    let (a, b) = (load(old)?, load(new)?);
    let mut v: Vec<Change> = a.keys().chain(b.keys()).collect::<std::collections::BTreeSet<_>>().into_iter()
        .map(|p| Change { path: p.clone(), before: a.get(p).copied(), after: b.get(p).copied() })
        .filter(|c| c.before != c.after)
        .collect();
    v.sort_by(|x, y| y.delta().abs().cmp(&x.delta().abs()).then_with(|| x.path.cmp(&y.path)));
    Ok(v)
}

pub fn text(v: &[Change], limit: usize) -> String {
    let mut s = String::new();
    if v.is_empty() {
        s.push_str("no change in any entry the two reports both list\n");
    }
    for c in v.iter().take(limit) {
        let d = c.delta();
        let sign = if d >= 0 { "+" } else { "-" };
        let what = match (c.before, c.after) {
            (None, _) => "new",
            (_, None) => "gone",
            _ => if d >= 0 { "grew" } else { "shrank" },
        };
        s.push_str(&format!("{sign}{:>10}  {what:<6}  {}\n", human(d.unsigned_abs() as u64), c.path));
    }
    if v.len() > limit {
        s.push_str(&format!("... {} more changes (-n to show more)\n", v.len() - limit));
    }
    s
}

pub fn json(v: &[Change]) -> Value {
    Value::Array(v.iter().map(|c| json!({"path": c.path, "before": c.before, "after": c.after,
                                         "delta": c.delta() as i64})).collect())
}

// ------------------------------------------------------------------------------------------------- compare --tree
// The same comparison, kept in the shape of the folders (plan 4.1 item 23): a cleanup's before/after at a glance,
// where each change sits under the folder that holds it. Only entries that changed, and the folders on the way to
// them, are shown.

/// One entry of the before/after tree. `None` = absent from that report.
pub struct Node {
    pub name: String,
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn delta(&self) -> i128 {
        self.after.unwrap_or(0) as i128 - self.before.unwrap_or(0) as i128
    }

    /// new, gone, grew, shrank, or same (a folder whose total did not move while things inside it did)
    pub fn what(&self) -> &'static str {
        match (self.before, self.after) {
            (None, _) => "new",
            (_, None) => "gone",
            _ if self.delta() > 0 => "grew",
            _ if self.delta() < 0 => "shrank",
            _ => "same",
        }
    }
}

/// Merge one entry of the old tree with the entry of the same name in the new one. Children are matched by
/// name. None when neither the entry nor anything under it changed.
fn merge(a: Option<&Value>, b: Option<&Value>) -> Option<Node> {
    let name = a.or(b).and_then(|v| v["name"].as_str()).unwrap_or("?").to_string();
    let before = a.map(|v| v["bytes"].as_u64().unwrap_or(0));
    let after = b.map(|v| v["bytes"].as_u64().unwrap_or(0));
    let kids = |v: Option<&Value>| -> BTreeMap<String, Value> {
        v.and_then(|v| v["children"].as_array()).map(|k| k.iter()
            .map(|c| (c["name"].as_str().unwrap_or("?").to_string(), c.clone())).collect()).unwrap_or_default()
    };
    let (ka, kb) = (kids(a), kids(b));
    let names: std::collections::BTreeSet<&String> = ka.keys().chain(kb.keys()).collect();
    let mut children: Vec<Node> = names.into_iter().filter_map(|n| merge(ka.get(n), kb.get(n))).collect();
    children.sort_by(|x, y| y.delta().abs().cmp(&x.delta().abs()).then_with(|| x.name.cmp(&y.name)));
    if before == after && children.is_empty() {
        return None;
    }
    Some(Node { name, before, after, children })
}

/// Before/after as trees, one per root (roots matched by their measured path).
pub fn tree_diff(old: &str, new: &str) -> Result<Vec<Node>, String> {
    let index = |roots: Vec<Value>| -> BTreeMap<String, Value> {
        roots.into_iter().map(|r| (r["path"].as_str().unwrap_or("?").to_string(), r["tree"].clone())).collect()
    };
    let (a, b) = (index(load_roots(old)?), index(load_roots(new)?));
    let paths: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    Ok(paths.into_iter().filter_map(|p| merge(a.get(p), b.get(p))).collect())
}

/// The tree as text, `per_level` changes per folder (the rest summed in one line), plain characters with `ascii`.
pub fn tree_text(roots: &[Node], per_level: usize, ascii: bool) -> String {
    fn line(n: &Node, prefix: &str, out: &mut String) {
        let d = n.delta();
        out.push_str(&format!("{}{:>10}  {:<6}  {prefix}{}\n", if d >= 0 { "+" } else { "-" },
                              human(d.unsigned_abs() as u64), n.what(), n.name));
    }
    fn kids(n: &Node, prefix: &str, per_level: usize, ascii: bool, out: &mut String) {
        let (tee, ell, pipe, gap) = if ascii { ("|-- ", "`-- ", "|   ", "    ") } else { ("├─ ", "└─ ", "│  ", "   ") };
        let shown = n.children.len().min(per_level);
        let rest = &n.children[shown..];
        for (i, c) in n.children[..shown].iter().enumerate() {
            let last = i + 1 == shown && rest.is_empty();
            line(c, &format!("{prefix}{}", if last { ell } else { tee }), out);
            kids(c, &format!("{prefix}{}", if last { gap } else { pipe }), per_level, ascii, out);
        }
        if !rest.is_empty() {
            let d: i128 = rest.iter().map(|c| c.delta()).sum();
            out.push_str(&format!("{}{:>10}  {:<6}  {prefix}{ell}... {} more changes (-n to show more)\n",
                                  if d >= 0 { "+" } else { "-" }, human(d.unsigned_abs() as u64), "", rest.len()));
        }
    }
    let mut s = String::new();
    if roots.is_empty() {
        s.push_str("no change in any entry the two reports both list\n");
    }
    for r in roots {
        line(r, "", &mut s);
        kids(r, "", per_level, ascii, &mut s);
    }
    s
}

/// The tree as nested JSON: name, before, after, delta, change, children.
pub fn tree_json(roots: &[Node]) -> Value {
    fn one(n: &Node) -> Value {
        let mut v = json!({"name": n.name, "before": n.before, "after": n.after, "delta": n.delta() as i64,
                           "change": n.what()});
        if !n.children.is_empty() {
            v["children"] = Value::Array(n.children.iter().map(one).collect());
        }
        v
    }
    json!({"roots": roots.iter().map(one).collect::<Vec<_>>()})
}
