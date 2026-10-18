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
