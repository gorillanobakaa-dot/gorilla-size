//! Patterns and whole folders in copy/move/recycle (2026-10-25). Gemma 4 12B asked to move a folder's files to the
//! trash gave `...\bench-playground\*`; the `*` was taken literally ("does not exist"), so it widened the request to
//! the folder itself, which also held a .txt it was never asked about; that failed only because the folder was its
//! own working folder ("os error 32", unexplained).

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
    let p = std::env::temp_dir().join(format!("gorilla-size-v04-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    Tmp(p)
}

/// shot1-3.png and notes.txt at the top, sub/deep.png one level down
fn playground(t: &Tmp) -> PathBuf {
    let d = t.0.join("bench-playground");
    fs::create_dir_all(d.join("sub")).unwrap();
    for i in 1..=3 {
        fs::write(d.join(format!("shot{i}.png")), format!("png {i}")).unwrap();
    }
    fs::write(d.join("notes.txt"), b"not asked about").unwrap();
    fs::write(d.join("sub").join("deep.png"), b"deeper").unwrap();
    d
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn pat(d: &Path, tail: &str) -> String {
    format!("{}{}{tail}", d.display(), std::path::MAIN_SEPARATOR)
}

fn run_in(dir: Option<&Path>, args: &[&str]) -> (i32, String) {
    let mut c = Command::new(BIN);
    c.args(args);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    let o = c.output().unwrap();
    (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn json_rc(args: &[&str]) -> (i32, Value) {
    let mut a = args.to_vec();
    a.push("--json");
    let (rc, out) = run_in(None, &a);
    (rc, serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}")))
}

/// The plan a `--plan` run wrote: the exact set of items the action would touch, nothing touched yet.
fn planned(args: &[&str]) -> (Vec<String>, Value) {
    let mut a = args.to_vec();
    a.push("--plan");
    let (rc, v) = json_rc(&a);
    assert_eq!(rc, 4, "{v}");
    let plan: Value = serde_json::from_str(&fs::read_to_string(v["plan"].as_str().unwrap()).unwrap()).unwrap();
    let _ = fs::remove_file(v["plan"].as_str().unwrap());
    let mut items: Vec<String> = plan["items"].as_array().unwrap().iter()
        .map(|i| Path::new(i["path"].as_str().unwrap()).file_name().unwrap().to_string_lossy().into_owned()).collect();
    items.sort();
    (items, v)
}

/// Once per run: the tests run in parallel and one probe file per process is shared (first version raced: os error 2)
fn bin_available() -> bool {
    static ONCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| {
        let t = std::env::temp_dir().join(format!("gorilla-size-v04-binprobe-{}", std::process::id()));
        fs::write(&t, b"probe").unwrap();
        let (rc, v) = json_rc(&["recycle", s(&t)]);
        if rc == 0 {
            let (urc, _) = json_rc(&["undo", v["journal"].as_str().unwrap()]);
            assert_eq!(urc, 0, "the probe went to the bin and must come back");
            let _ = fs::remove_file(&t);
            return true;
        }
        let _ = fs::remove_file(&t);
        let out = v.to_string();
        assert!(out.contains("no Recycle Bin folder") || out.contains("no home folder"), "unexpected refusal: {out}");
        eprintln!("SKIPPED (no Recycle Bin for this account on this machine): {out}");
        false
    })
}

#[test]
fn a_star_recycles_each_matching_file_in_that_folder_only_and_each_is_found_in_the_bin() {
    if !bin_available() {
        return;
    }
    let t = tmp("star");
    let d = playground(&t);
    let (rc, v) = json_rc(&["recycle", &pat(&d, "*.png")]);
    assert_eq!(rc, 0, "{v}");
    let acts = v["actions"].as_array().unwrap();
    assert_eq!(acts.len(), 3, "the three top-level PNGs, listed one by one: {v}");
    for a in acts {
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(a["why"], "found in the bin's own records", "{a}");
        assert!(Path::new(a["to"].as_str().unwrap()).exists(), "in the bin: {a}");
    }
    assert!(v["notes"][0].as_str().unwrap().contains("matched 3 file(s)"), "{v}");
    assert!(!d.join("shot1.png").exists());
    assert!(d.join("notes.txt").exists(), "a file the pattern does not match is untouched");
    assert!(d.join("sub").join("deep.png").exists(), "not recursive unless the pattern says so");
    let (urc, u) = json_rc(&["undo", v["journal"].as_str().unwrap()]);
    assert_eq!(urc, 0, "{u}");
    assert_eq!(fs::read(d.join("shot2.png")).unwrap(), b"png 2");
}

#[test]
fn a_pattern_that_matches_nothing_says_matched_nothing_and_what_is_there() {
    let t = tmp("none");
    let d = playground(&t);
    let (rc, v) = json_rc(&["recycle", &pat(&d, "*.jpg")]);
    assert_eq!(rc, 3, "{v}");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("matched nothing, nothing touched"), "{e}");
    assert!(!e.contains("does not exist"), "the folder exists: {e}");
    assert!(e.contains("shot1.png") && e.contains("notes.txt"), "what is there, to correct the pattern: {e}");
    let (rc, v) = json_rc(&["recycle", &pat(&d.join("nope"), "*")]);
    assert_eq!(rc, 3);
    assert!(v["error"].as_str().unwrap().contains("matched nothing") && v["error"].as_str().unwrap()
        .contains("nope does not exist"), "{v}");
}

#[test]
fn one_pattern_matching_nothing_stops_the_whole_call() {
    let t = tmp("partial");
    let d = playground(&t);
    let (rc, v) = json_rc(&["recycle", &pat(&d, "*.png"), &pat(&d, "*.jpg")]);
    assert_eq!(rc, 3, "{v}");
    assert!(d.join("shot1.png").exists(), "nothing touched");
}

#[test]
fn a_star_matches_files_never_folders_and_says_which_folders_it_left() {
    let t = tmp("dirs");
    let d = playground(&t);
    let (items, v) = planned(&["recycle", &pat(&d, "*")]);
    assert_eq!(items, ["notes.txt", "shot1.png", "shot2.png", "shot3.png"]);
    let n = v["notes"][0].as_str().unwrap();
    assert!(n.contains("matched 4 file(s)") && n.contains("left alone") && n.contains("sub"), "{n}");
}

#[test]
fn question_mark_and_double_star_reach_where_the_pattern_says() {
    let t = tmp("deep");
    let d = playground(&t);
    let (items, _) = planned(&["recycle", &pat(&d, "shot?.png")]);
    assert_eq!(items, ["shot1.png", "shot2.png", "shot3.png"]);
    let (items, _) = planned(&["recycle", &pat(&d, &format!("**{}*.png", std::path::MAIN_SEPARATOR))]);
    assert_eq!(items, ["deep.png", "shot1.png", "shot2.png", "shot3.png"]);
    let (items, _) = planned(&["recycle", &pat(&d, "**")]);
    assert_eq!(items.len(), 5, "{items:?}");
    let (items, _) = planned(&["recycle", &pat(&d, &format!("s*{}*.png", std::path::MAIN_SEPARATOR))]);
    assert_eq!(items, ["deep.png"], "a pattern in a folder place matches folders");
}

#[test]
fn a_folder_part_that_exists_as_written_is_taken_as_written() {
    let t = tmp("bracket");
    let d = t.0.join("photos [1]");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("a.png"), b"a").unwrap();
    let (items, _) = planned(&["recycle", &pat(&d, "*.png")]);
    assert_eq!(items, ["a.png"]);
}

#[test]
fn a_folder_holding_files_is_refused_with_how_many_and_which_unless_whole_folder() {
    let t = tmp("whole");
    let d = playground(&t);
    let (rc, v) = json_rc(&["recycle", s(&d)]);
    assert_eq!(rc, 3, "{v}");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("recycles the whole folder, 5 file(s)"), "{e}");
    assert!(e.contains("notes.txt") && e.contains("shot1.png"), "names the files: {e}");
    assert!(e.contains("Refused, nothing touched") && e.contains("--whole-folder"), "{e}");
    assert!(e.contains(&pat(&d, "*.png").replace('\\', "\\")), "suggests the commonest extension: {e}");
    assert!(d.join("notes.txt").exists());
    let (items, v) = planned(&["recycle", s(&d), "--whole-folder"]);
    assert_eq!(items, ["bench-playground"]);
    assert!(v["notes"][0].as_str().unwrap().contains("this recycles all 5 file(s) in it"), "{v}");
}

#[test]
fn an_empty_folder_needs_no_flag_and_copy_or_move_of_a_folder_is_not_refused() {
    let t = tmp("empty");
    let e = t.0.join("empty");
    fs::create_dir_all(&e).unwrap();
    let (items, _) = planned(&["recycle", s(&e)]);
    assert_eq!(items, ["empty"]);
    let d = playground(&t);
    let to = t.0.join("to");
    fs::create_dir_all(&to).unwrap();
    let (items, _) = planned(&["move", s(&d), "--to", s(&to)]);
    assert_eq!(items, ["bench-playground"]);
}

#[test]
fn copy_takes_patterns_too() {
    let t = tmp("copy");
    let d = playground(&t);
    let to = t.0.join("to");
    fs::create_dir_all(&to).unwrap();
    let (rc, v) = json_rc(&["copy", &pat(&d, "*.txt"), "--to", s(&to)]);
    assert_eq!(rc, 0, "{v}");
    assert!(to.join("notes.txt").exists() && !to.join("shot1.png").exists());
}

#[cfg(unix)]
#[test]
fn a_real_name_with_a_star_wins_over_the_pattern() {
    let t = tmp("literal");
    fs::write(t.0.join("a*b"), b"x").unwrap();
    fs::write(t.0.join("aXb"), b"x").unwrap();
    let (items, v) = planned(&["recycle", s(&t.0.join("a*b"))]);
    assert_eq!(items, ["a*b"]);
    assert!(v["notes"].is_null(), "not expanded: {v}");
}

#[cfg(windows)]
#[test]
fn recycling_the_folder_the_caller_works_in_is_refused_in_plain_words() {
    let t = tmp("cwd");
    let d = playground(&t);
    let (rc, out) = run_in(Some(&d.join("sub")), &["recycle", s(&d), "--whole-folder", "--json"]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("REFUSED, nothing touched") && out.contains("is in use: it is the working folder"), "{out}");
    assert!(out.contains("Recycle its contents instead") && out.contains("bench-playground\\\\*"), "{out}");
    assert!(d.join("notes.txt").exists());
}

#[cfg(windows)]
#[test]
fn a_folder_another_program_works_in_explains_error_32() {
    if !bin_available() {
        return;
    }
    let t = tmp("busy");
    let d = playground(&t);
    // another program whose working folder is d: Windows refuses to move d (ERROR_SHARING_VIOLATION)
    let mut other = Command::new("ping").args(["-n", "30", "127.0.0.1"]).current_dir(&d)
        .stdout(std::process::Stdio::null()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (rc, out) = run_in(None, &["recycle", s(&d), "--whole-folder", "--json"]);
    let _ = other.kill(); // by its own handle: only the process this test started
    let _ = other.wait();
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("is in use by another program (Windows error 32)"), "{out}");
    assert!(out.contains("working folder") && out.contains("recycle its contents instead"), "{out}");
    assert!(!out.contains("os error 32"), "plain words, not the raw error: {out}");
    assert!(d.join("notes.txt").exists(), "nothing touched");
}
