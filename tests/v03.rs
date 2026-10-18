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
#[test]
fn find_format_prints_a_file_named_like_a_placeholder_as_its_name() {
    // 2026-10-18: `{bytes}.txt` printed as `5.txt`: the value of {name} was searched for placeholders again
    let t = tmp("fmt");
    file(&t.0.join("{bytes}.txt"), b"12345");
    file(&t.0.join("{size} and {path}.log"), b"1");
    let (rc, out, err) = run(&["find", s(&t.0), "--type", "f", "--sort", "name", "--format", "{name}={bytes}"]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out.lines().collect::<Vec<_>>(), vec!["{bytes}.txt=5", "{size} and {path}.log=1"], "{out}");
}
/// Run to the end; return (exit code, stdout, the child's peak private memory in bytes).
/// Windows: PeakPagefileUsage from K32GetProcessMemoryInfo (still readable after the exit while the handle is
/// open). Linux: getrusage(RUSAGE_CHILDREN) max RSS (the largest child of this test process so far; every other
/// child the tests start walks a few files and stays far below the limits used here).
fn run_measured(args: &[&str]) -> (i32, String, u64) {
    use std::io::Read;
    let mut child = Command::new(BIN).args(args).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    let rc = child.wait().unwrap().code().unwrap_or(-1);
    (rc, out, peak_memory(&child))
}

#[cfg(windows)]
fn peak_memory(child: &std::process::Child) -> u64 {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Counters { cb: u32, faults: u32, peak_ws: usize, ws: usize, qpp_peak: usize, qpp: usize, qnp_peak: usize,
                      qnp: usize, pagefile: usize, peak_pagefile: usize }
    #[link(name = "kernel32")]
    extern "system" {
        fn K32GetProcessMemoryInfo(h: *mut std::ffi::c_void, c: *mut Counters, cb: u32) -> i32;
    }
    let mut c = Counters { cb: std::mem::size_of::<Counters>() as u32, ..Default::default() };
    assert_ne!(unsafe { K32GetProcessMemoryInfo(child.as_raw_handle() as _, &mut c, c.cb) }, 0);
    c.peak_pagefile as u64
}

#[cfg(target_os = "linux")]
fn peak_memory(_child: &std::process::Child) -> u64 {
    #[repr(C)]
    struct Rusage { utime: [i64; 2], stime: [i64; 2], maxrss: i64, rest: [i64; 13] }
    extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    let mut r = Rusage { utime: [0; 2], stime: [0; 2], maxrss: 0, rest: [0; 13] };
    assert_eq!(unsafe { getrusage(-1, &mut r) }, 0); // RUSAGE_CHILDREN
    r.maxrss as u64 * 1024
}

#[cfg(not(any(windows, target_os = "linux")))]
fn peak_memory(_child: &std::process::Child) -> u64 {
    0 // not measured here; the output checks still run
}

#[test]
fn a_huge_file_without_line_ends_is_read_only_up_to_max_bytes() {
    // 2026-10-18 audit: --head/--grep read a whole line before capping it; a 64 MiB file with no newline was
    // pulled into memory in full (twice: bytes, then text)
    use std::io::Write;
    let t = tmp("noline");
    let mut f = fs::File::create(t.0.join("dump.txt")).unwrap();
    let chunk = vec![b'a'; 1 << 20];
    for _ in 0..64 {
        f.write_all(&chunk).unwrap();
    }
    drop(f);
    const LIMIT: u64 = 32 << 20;
    let (rc, out, peak) = run_measured(&["find", s(&t.0), "--ext", "txt", "--head", "1", "--max-bytes", "4K"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("(truncated at 4096 bytes"), "{out}");
    let shown = out.lines().find(|l| l.contains("1| a")).unwrap_or_else(|| panic!("{out}"));
    assert!(shown.len() <= 4096 + 16, "{} bytes shown", shown.len());
    assert!(peak < LIMIT, "--head 1 peaked at {peak} bytes");
    let (rc, out, peak) = run_measured(&["find", s(&t.0), "--ext", "txt", "--grep", "aaa", "--max-bytes", "4K"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("1 line(s) longer than --max-bytes: truncated at 4096 bytes"), "{out}");
    assert!(peak < LIMIT, "--grep peaked at {peak} bytes");
    let (rc, out, peak) = run_measured(&["find", s(&t.0), "--ext", "txt", "--tail", "1", "--max-bytes", "4K"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("truncated at 4096 bytes"), "{out}");
    assert!(peak < LIMIT, "--tail 1 peaked at {peak} bytes");
}

#[test]
fn a_binary_file_is_summarized_with_size_kind_and_first_bytes() {
    let t = tmp("binsum");
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.resize(100, 0);
    file(&t.0.join("a-image.png"), &png);
    let mut exe = b"MZ\x90\0".to_vec();
    exe.resize(64, 0);
    file(&t.0.join("b-program.log"), &exe); // the name says log; the bytes say Windows program
    file(&t.0.join("c-blob.dat"), &[0, 1, 2, 3]);
    let (rc, out, err) = run(&["find", s(&t.0), "--type", "f", "--sort", "name", "--head", "1"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("binary file: not shown; 100 B, PNG, first 16 bytes 89 50 4e 47 0d 0a 1a 0a 00 00 00 0d 49 48 44 52"),
            "{out}");
    assert!(out.contains("64 B, PE/MZ, first 16 bytes 4d 5a 90 00"), "{out}");
    assert!(out.contains("4 B, kind unknown, first 4 bytes 00 01 02 03"), "{out}");
    let (_, out, _) = run(&["find", s(&t.0), "--name", "a-image.png", "--head", "1", "--jsonl"]);
    let v: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(v["content"]["binary"], true);
    assert_eq!(v["content"]["kind"], "PNG");
    assert_eq!(v["content"]["bytes"], 100);
    assert_eq!(v["content"]["first_bytes"], "89504e470d0a1a0a0000000d49484452");
}

#[test]
fn find_hashes_only_what_grep_kept() {
    let t = tmp("hashgrep");
    file(&t.0.join("a.log"), b"all fine\n");
    file(&t.0.join("b.log"), b"ERROR disk full\n");
    file(&t.0.join("c.log"), b"fine too\n");
    file(&t.0.join("sub/d.log"), b"nothing\n");
    let (_, want) = json_rc(&["hash", s(&t.0.join("b.log"))]);
    let (rc, v) = json_rc(&["find", s(&t.0), "--ext", "log", "--grep", "ERROR", "--hash"]);
    assert_eq!(rc, 0, "{v}");
    let found = v["roots"][0]["found"].as_array().unwrap();
    assert_eq!(found.len(), 1, "grep selects: {v}");
    assert!(found[0]["path"].as_str().unwrap().ends_with("b.log"));
    assert_eq!(found[0]["sha256"], want[0]["sha256"]);
    assert_eq!(v["grep_dropped"], 3, "{v}");
    assert_eq!(v["hashed"], 1, "nothing grep dropped was hashed: {v}");
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "--grep", "ERROR", "--hash"]);
    let sha = want[0]["sha256"].as_str().unwrap();
    assert!(out.lines().any(|l| l.contains(sha) && l.ends_with("b.log")), "{out}");
    assert!(!out.contains("a.log") && !out.contains("c.log"), "{out}");
    let (_, out, _) = run(&["find", s(&t.0), "--name", "b.log", "--hash", "--format", "{sha256}  {name}"]);
    assert_eq!(out.trim_end(), format!("{sha}  b.log"));
}
#[test]
fn dupes_empty_reparse_exit_3_when_they_find_something_and_0_when_clean() {
    let t = tmp("exit3");
    let clean = t.0.join("clean");
    file(&clean.join("a.txt"), b"one");
    file(&clean.join("b.txt"), b"two");
    for mode in ["dupes", "empty", "reparse"] {
        let (rc, v) = json_rc(&[mode, s(&clean)]);
        assert_eq!(rc, 0, "{mode} on a clean folder: {v}");
    }
    let dirty = t.0.join("dirty");
    file(&dirty.join("a.bin"), &[7u8; 100]);
    file(&dirty.join("copy/a.bin"), &[7u8; 100]);
    file(&dirty.join("zero.txt"), b"");
    let (rc, _) = json_rc(&["dupes", s(&dirty)]);
    assert_eq!(rc, 3);
    let (rc, _) = json_rc(&["empty", s(&dirty)]);
    assert_eq!(rc, 3);
    // a link whose target is gone is a finding; a working link is not
    let target = dirty.join("target");
    fs::create_dir_all(&target).unwrap();
    let link = dirty.join("link");
    #[cfg(windows)]
    let made = Command::new("cmd").args(["/C", "mklink", "/J", s(&link), s(&target)]).output()
        .map(|o| o.status.success()).unwrap_or(false);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &link).is_ok();
    assert!(made, "could not make the link for the test");
    let (rc, v) = json_rc(&["reparse", s(&dirty)]);
    assert_eq!(rc, 0, "a working link: {v}");
    fs::remove_dir(&target).unwrap();
    let (rc, v) = json_rc(&["reparse", s(&dirty)]);
    assert_eq!(rc, 3, "a broken link: {v}");
    assert_eq!(v[0]["target_exists"], false);
}

#[test]
fn under_json_an_error_is_json_on_stdout() {
    let gone = std::env::temp_dir().join("gorilla-size-v03-no-such-folder-anywhere");
    for args in [vec!["find", s(&gone)], vec![s(&gone)], vec!["empty", s(&gone)], vec!["dupes", s(&gone)],
                 vec!["hash", s(&gone)], vec!["--nonsense"], vec!["compare", "only-one.json"]] {
        let (rc, v) = json_rc(&args);
        assert_eq!(rc, 2, "{args:?}: {v}");
        assert_eq!(v["exit"], 2, "{args:?}: {v}");
        assert!(!v["error"].as_str().unwrap().is_empty(), "{args:?}: {v}");
    }
    // without --json: a sentence on stderr, nothing on stdout
    let (rc, out, err) = run(&["find", s(&gone)]);
    assert_eq!((rc, out.as_str()), (2, ""));
    assert!(err.contains("does not exist"), "{err}");
}
#[test]
fn long_shows_the_owner_of_each_entry() {
    let t = tmp("owner");
    file(&t.0.join("mine.txt"), b"x");
    file(&t.0.join("sub/inner.txt"), b"xy");
    let (rc, out, err) = run(&[s(&t.0), "--long", "-d", "2", "--ascii"]);
    assert_eq!(rc, 0, "{err}");
    // ... attrs DATE  OWNER  |-- name   (the owner sits between the date and the tree branch)
    let re = regex::Regex::new(r"\d{4}-\d{2}-\d{2}  (\S.*?)\s{2,}(?:\|   |    )*(?:\|-- |`-- )(\S+)").unwrap();
    for name in ["mine.txt", "inner.txt", "sub/"] {
        let line = out.lines().find(|l| l.contains(name)).unwrap_or_else(|| panic!("{name}:\n{out}"));
        let c = re.captures(line).unwrap_or_else(|| panic!("no owner column: {line}"));
        let owner = c[1].trim();
        assert!(!owner.is_empty() && owner != "?", "owner of {name}: {owner:?} in {line}");
    }
    let (_, flat, _) = run(&[s(&t.0), "--long", "--flat", "-d", "2"]);
    assert!(flat.lines().filter(|l| l.contains("inner.txt")).all(|l| !l.contains(" ?  ")), "{flat}");
}