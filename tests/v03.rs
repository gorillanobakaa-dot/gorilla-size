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