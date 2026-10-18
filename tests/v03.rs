//! v0.3 through the built binary (plan section 4.1 finished, 2026-10-18): which SHA-256 path ran, the profile
//! measured against every core, watch alerts, compare as a tree, find's format/read/hash fixes, the exit-code
//! contract (3 = something found), JSON errors, the owner in --long.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_gorilla-size");

struct Tmp(PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tmp(tag: &str) -> Tmp {
    let p = std::env::temp_dir().join(format!("gorilla-size-v03-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    Tmp(p)
}

fn file(p: &Path, body: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(BIN).args(args).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

/// Run with `--json` added; return the exit code and the parsed output.
fn json_rc(args: &[&str]) -> (i32, Value) {
    let mut a = args.to_vec();
    a.push("--json");
    let (rc, out, err) = run(&a);
    (rc, serde_json::from_str(&out).unwrap_or_else(|e| panic!("rc {rc} {e}: {err}{out}")))
}

#[test]
fn profile_says_which_sha256_path_hash_uses() {
    let (rc, v) = json_rc(&["profile"]);
    assert_eq!(rc, 0, "{v}");
    let imp = v["sha256_impl"].as_str().unwrap();
    let t = tmp("prof");
    file(&t.0.join("x"), b"abc");
    let (_, h) = json_rc(&["hash", s(&t.0.join("x"))]);
    assert_eq!(h[0]["sha256_impl"], imp, "profile and hash name the same path");
    assert_eq!(h[0]["sha256"], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    let (_, text, _) = run(&["profile"]);
    assert!(text.contains(&format!("SHA-256  {imp}")), "{text}");
}

/// Run with an empty state folder: no machine.json, no journal of the owner's touched.
fn run_fresh_state(args: &[&str], state: &Path) -> (i32, String, String) {
    let o = Command::new(BIN).args(args).env("LOCALAPPDATA", state).env("XDG_STATE_HOME", state).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

#[test]
fn bench_profile_measures_the_calibrated_count_against_every_core() {
    let t = tmp("benchprof");
    for i in 0..300 {
        file(&t.0.join(format!("d{}/f{i}", i % 30)), b"x");
    }
    let (rc, v) = json_rc(&["bench", "--profile", s(&t.0), "-j", "1", "--rounds", "3"]);
    assert_eq!(rc, 0, "{v}");
    assert_eq!(v["profile_threads"], 1);
    assert_eq!(v["threads_from"], "asked (-j)");
    assert_eq!(v["profile_seconds"].as_array().unwrap().len(), 3);
    assert_eq!(v["all_seconds"].as_array().unwrap().len(), 3);
    for side in ["profile", "all"] {
        let sp = &v[side];
        let (lo, q1, med, q3, hi) = (sp["min"].as_f64().unwrap(), sp["q1"].as_f64().unwrap(),
                                     sp["median"].as_f64().unwrap(), sp["q3"].as_f64().unwrap(),
                                     sp["max"].as_f64().unwrap());
        assert!(lo <= q1 && q1 <= med && med <= q3 && q3 <= hi, "{side}: {sp}");
    }
    let verdict = v["verdict"].as_str().unwrap();
    assert!(verdict.starts_with("INCONCLUSIVE") || verdict.contains("FASTER"), "{verdict}");
    // no calibration for this disk and no -j: nothing to test, refused (fail closed), not a silent "every core"
    let state = t.0.join("state");
    fs::create_dir_all(&state).unwrap();
    let (rc, out, err) = run_fresh_state(&["bench", "--profile", s(&t.0)], &state);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(err.contains("no calibrated thread count"), "{err}");
}

#[test]
fn bench_runs_ten_rounds_unless_told_otherwise() {
    // plan item 300: the default was 3 rounds
    // bench's own --json goes before `--`; after it everything belongs to the two programs
    let (rc, out, err) = run(&["bench", BIN, BIN, "--json", "--", "--version"]);
    assert_eq!(rc, 0, "{err}{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["rounds"], 10);
    assert_eq!(v["a_seconds"].as_array().unwrap().len(), 10);
    assert!(v["a_spread"]["max"].as_f64().unwrap() >= v["a_spread"]["min"].as_f64().unwrap());
}
/// watch with stdout read line by line: `grow` runs once, right after the first measurement is printed, so the
/// growth always falls between measurement 1 and 2 (no sleep-based race with process start-up).
fn watch_lines(args: &[&str], grow: impl FnOnce()) -> (i32, Vec<Value>) {
    use std::io::BufRead;
    let mut child = Command::new(BIN).args(args).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let mut out = vec![serde_json::from_str::<Value>(&lines.next().unwrap().unwrap()).unwrap()];
    grow();
    for l in lines {
        out.push(serde_json::from_str(&l.unwrap()).unwrap());
    }
    (child.wait().unwrap().code().unwrap_or(-1), out)
}

#[test]
fn watch_alerts_on_growth_over_the_threshold_and_stops_with_exit_3() {
    let t = tmp("watchalert");
    file(&t.0.join("seed.txt"), b"x");
    let big = t.0.join("big.bin");
    let (rc, v) = watch_lines(&["watch", s(&t.0), "--every", "1", "--count", "3", "--alert-growth", "1M", "--json"],
                              || file(&big, &vec![0u8; 2 << 20]));
    assert_eq!(rc, 3, "{v:?}");
    assert_eq!(v.len(), 2, "stops at the first alert: {v:?}");
    assert!(v[0].get("alert").is_none(), "{}", v[0]);
    assert_eq!(v[1]["alert"], true, "{}", v[1]);
    assert!(v[1]["delta"].as_i64().unwrap() >= 2 << 20, "{}", v[1]);
    // under the threshold: no alert, runs to --count, exit 0
    let (rc, v) = watch_lines(&["watch", s(&t.0), "--every", "1", "--count", "2", "--alert-growth", "1G", "--json"],
                              || file(&t.0.join("more.bin"), &vec![0u8; 4096]));
    assert_eq!(rc, 0, "{v:?}");
    assert!(v.iter().all(|x| x.get("alert").is_none()), "{v:?}");
    let (_, text, _) = run(&["watch", s(&t.0), "--count", "1"]);
    assert!(!text.contains("ALERT"), "{text}");
}

#[test]
fn watch_honors_exclude_like_a_measurement() {
    // 2026-10-18 audit: watch measured with totals_only() and ignored --exclude/--gitignore/--no-hidden
    let t = tmp("watchex");
    file(&t.0.join("keep.txt"), b"12345");
    let (rc, v) = watch_lines(&["watch", s(&t.0), "--every", "1", "--count", "2", "--alert-growth", "1M",
                                "--exclude", "*.bin", "--json"],
                              || file(&t.0.join("excluded.bin"), &vec![0u8; 2 << 20]));
    assert_eq!(rc, 0, "growth only in excluded files is no growth: {v:?}");
    assert_eq!(v[0]["bytes"], 5);
    assert_eq!(v[1]["bytes"], 5, "{}", v[1]);
    assert_eq!(v[1]["excluded"], 1, "{}", v[1]);
    assert_eq!(v[1]["delta"], 0);
}
#[test]
fn compare_tree_shows_each_change_under_its_folder_text_and_nested_json() {
    let t = tmp("cmptree");
    let tree = t.0.join("tree");
    file(&tree.join("a.bin"), &vec![1u8; 1000]);
    file(&tree.join("big/b.patch"), &vec![1u8; 2000]);
    file(&tree.join("small/d.txt"), &vec![1u8; 500]);
    let (before, after) = (t.0.join("before.json"), t.0.join("after.json"));
    let (_, out, _) = run(&[s(&tree), "-d", "3", "--json"]);
    fs::write(&before, out).unwrap();
    file(&tree.join("big/new.bin"), &vec![1u8; 7000]);
    fs::remove_file(tree.join("small/d.txt")).unwrap();
    let (_, out, _) = run(&[s(&tree), "-d", "3", "--json"]);
    fs::write(&after, out).unwrap();

    let (rc, text, err) = run(&["compare", s(&before), s(&after), "--tree", "--ascii"]);
    assert_eq!(rc, 0, "{err}");
    assert!(text.is_ascii(), "{text}");
    let lines: Vec<&str> = text.lines().collect();
    let at = |needle: &str| lines.iter().position(|l| l.ends_with(needle)).unwrap_or_else(|| panic!("{needle}:\n{text}"));
    let (root, big, new, small, gone) = (at("tree"), at("-- big"), at("-- new.bin"), at("-- small"), at("-- d.txt"));
    assert!(lines[root].contains("grew"), "{text}");
    assert!(lines[big].contains("grew") && lines[new].contains("new") && lines[gone].contains("gone"), "{text}");
    assert!(root < big && big < new && new < small && small < gone, "children under their folder:\n{text}");
    // one level deeper than its folder: the folder's branch plus one more indent
    assert!(lines[new].find("new.bin").unwrap() > lines[big].find("big").unwrap(), "{text}");
    assert!(!text.contains("a.bin") && !text.contains("b.patch"), "unchanged entries are not shown:\n{text}");

    let (rc, v) = json_rc(&["compare", s(&before), s(&after), "--tree"]);
    assert_eq!(rc, 0, "{v}");
    let r = &v["roots"][0];
    assert_eq!(r["delta"], 6500);
    let kid = |n: &Value, name: &str| n["children"].as_array().unwrap().iter().find(|c| c["name"] == name).cloned()
        .unwrap_or_else(|| panic!("{name} in {n}"));
    let big = kid(r, "big");
    assert_eq!(big["change"], "grew");
    let nb = kid(&big, "new.bin");
    assert_eq!((nb["change"].as_str(), nb["before"].is_null(), nb["after"].as_u64()), (Some("new"), true, Some(7000)));
    assert_eq!(kid(&kid(r, "small"), "d.txt")["change"], "gone");
}