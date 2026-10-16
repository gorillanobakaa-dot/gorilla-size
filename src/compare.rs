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

fn load(path: &str) -> Result<BTreeMap<String, u64>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    // Windows PowerShell 5.1 writes UTF-8 with a byte-order mark (Set-Content -Encoding utf8, Out-File); JSON parsers
    // reject it. 2026-10-16: a report saved that way was refused as "not a gorilla-size JSON report".
    let text = text.trim_start_matches('\u{feff}');
    let v: Value = serde_json::from_str(text).map_err(|e| format!("{path}: not a gorilla-size JSON report: {e}"))?;
    let roots = v["roots"].as_array().ok_or(format!("{path}: no \"roots\" (make it with gorilla-size --json)"))?;
    let mut m = BTreeMap::new();
    for r in roots {
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
