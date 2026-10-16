//! v0.2 through the built binary: find output for the next step, reading inside matches, types, layouts, excludes,
//! and the actions - copy verified, move, recycle into the REAL Recycle Bin/trash and back, plans above a
//! threshold, undo, the refusals that stop Windows from destroying instead of recycling.

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
    let p = std::env::temp_dir().join(format!("gorilla-size-v02-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    Tmp(p)
}

fn file(p: &Path, body: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(BIN).args(args).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

fn json(args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    let (rc, out, err) = run(&a);
    assert!(rc == 0 || rc == 4, "rc {rc}: {err}{out}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn find_feeds_the_next_step_paths_nul_format_jsonl() {
    let t = tmp("out");
    file(&t.0.join("a.log"), b"one\ntwo\n");
    file(&t.0.join("sub/b.log"), b"three\n");
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "--paths", "--sort", "name"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("a.log") && Path::new(lines[0]).is_absolute(), "{out}");
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "-0"]);
    assert_eq!(out.matches('\0').count(), 2);
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "--format", "{name}={bytes}", "--sort", "name"]);
    assert_eq!(out.lines().collect::<Vec<_>>(), vec!["a.log=8", "b.log=6"]);
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "--jsonl"]);
    for l in out.lines() {
        let v: Value = serde_json::from_str(l).unwrap();
        assert!(v["bytes"].as_u64().unwrap() > 0);
    }
}

#[test]
fn read_inside_what_was_found_in_the_same_call() {
    let t = tmp("read");
    let body: String = (1..=50).map(|i| if i == 37 { "ERROR disk full\n".to_string() } else { format!("ok {i}\n") }).collect();
    file(&t.0.join("build.log"), body.as_bytes());
    file(&t.0.join("blob.bin"), &[0, 1, 2, 3]);
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "--grep", "error", "-i"]);
    assert!(out.contains("37: ERROR disk full"), "{out}");
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "--tail", "2"]);
    assert!(out.contains("    49| ok 49") && out.contains("    50| ok 50"), "{out}");
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "--lines", "10:11"]);
    assert!(out.contains("    10| ok 10") && out.contains("    11| ok 11") && !out.contains("ok 12"), "{out}");
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "bin", "--head", "3"]);
    assert!(out.contains("binary file: not shown"), "{out}");
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "log", "--grep", "ERROR", "--jsonl"]);
    let v: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(v["content"]["grep"][0]["line"], 37);
}

#[test]
fn types_layouts_and_long() {
    let t = tmp("types");
    file(&t.0.join("x/a.pcapng"), &vec![1u8; 9000]);
    file(&t.0.join("x/b.etl"), &vec![1u8; 3000]);
    file(&t.0.join("c.etl"), &vec![1u8; 1000]);
    let v = json(&["types", s(&t.0)]);
    let ty = v["roots"][0]["types"].as_array().unwrap();
    assert_eq!(ty[0]["ext"], "pcapng");
    assert_eq!(ty[1]["ext"], "etl");
    assert_eq!(ty[1]["bytes"], 4000);
    assert_eq!(ty[1]["files"], 2);
    let (_, out, _) = run(&[s(&t.0), "--reverse"]);
    let c = out.find("c.etl").unwrap();
    let x = out.find("x/").unwrap();
    assert!(c < x, "smallest first:\n{out}");
    let (_, out, _) = run(&[s(&t.0), "--flat", "-d", "2"]);
    assert!(out.contains("x/a.pcapng"), "{out}");
    let (_, out, _) = run(&[s(&t.0), "--long"]);
    assert!(out.lines().any(|l| l.contains("c.etl") && l.contains('-') && l.matches('-').count() >= 2), "{out}");
}

#[test]
fn exclude_gitignore_and_hidden_are_said_not_silent() {
    let t = tmp("ign");
    file(&t.0.join(".gitignore"), b"target/\n*.tmp\n");
    file(&t.0.join("src/main.rs"), &vec![1u8; 100]);
    file(&t.0.join("target/big.o"), &vec![1u8; 10_000]);
    file(&t.0.join("x.tmp"), &vec![1u8; 500]);
    file(&t.0.join(".hidden/h"), &vec![1u8; 50]);
    let all = json(&[s(&t.0)]);
    let ign = json(&[s(&t.0), "--gitignore"]);
    let gi_bytes = fs::metadata(t.0.join(".gitignore")).unwrap().len();
    assert_eq!(all["roots"][0]["totals"]["bytes"], 10_650 + gi_bytes);
    assert_eq!(ign["roots"][0]["totals"]["bytes"], 150 + gi_bytes);
    assert_eq!(ign["roots"][0]["totals"]["excluded"], 2);
    let ex = json(&[s(&t.0), "--exclude", "src", "--no-hidden"]);
    assert_eq!(ex["roots"][0]["totals"]["excluded"], 1);
    assert!(ex["roots"][0]["totals"]["hidden_skipped"].as_u64().unwrap() >= 2, "{}", ex["roots"][0]["totals"]);
    let (_, out, _) = run(&[s(&t.0), "--gitignore"]);
    assert!(out.contains("excluded by .gitignore/--exclude: 2"), "{out}");
}

#[test]
fn copy_is_verified_and_refuses_to_overwrite_then_keeps_the_old_aside() {
    let t = tmp("copy");
    file(&t.0.join("src/a/one.txt"), b"hello");
    file(&t.0.join("src/a/two.txt"), b"world");
    fs::create_dir_all(t.0.join("dst")).unwrap();
    let (rc, out, _) = run(&["copy", s(&t.0.join("src/a")), "--to", s(&t.0.join("dst"))]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("every byte compared"), "{out}");
    assert_eq!(fs::read(t.0.join("dst/a/two.txt")).unwrap(), b"world");
    let (rc, out, _) = run(&["copy", s(&t.0.join("src/a")), "--to", s(&t.0.join("dst"))]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("target exists"));
    file(&t.0.join("src/a/one.txt"), b"HELLO2");
    let (rc, out, _) = run(&["copy", s(&t.0.join("src/a")), "--to", s(&t.0.join("dst")), "--overwrite"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(fs::read(t.0.join("dst/a/one.txt")).unwrap(), b"HELLO2");
    let kept: Vec<_> = fs::read_dir(t.0.join("dst")).unwrap().flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("gorilla-bak")).collect();
    assert_eq!(kept.len(), 1, "the old copy is kept aside, never destroyed");
}

#[test]
fn move_on_one_drive_then_undo_puts_it_back() {
    let t = tmp("move");
    file(&t.0.join("from/thing/f.txt"), b"data");
    fs::create_dir_all(t.0.join("to")).unwrap();
    let v = json(&["move", s(&t.0.join("from/thing")), "--to", s(&t.0.join("to"))]);
    assert_eq!(v["failed"], 0, "{v}");
    assert!(t.0.join("to/thing/f.txt").exists() && !t.0.join("from/thing").exists());
    let journal = v["journal"].as_str().unwrap().to_string();
    let u = json(&["undo", &journal]);
    assert_eq!(u["failed"], 0, "{u}");
    assert!(t.0.join("from/thing/f.txt").exists() && !t.0.join("to/thing").exists());
}

#[test]
fn recycle_really_reaches_the_bin_and_undo_restores_it() {
    let t = tmp("bin");
    let victim = t.0.join(format!("gorilla-size-test-{}.txt", std::process::id()));
    file(&victim, b"put me in the bin");
    let v = json(&["recycle", s(&victim)]);
    assert_eq!(v["failed"], 0, "{v}");
    assert!(!victim.exists());
    let item = v["actions"][0]["to"].as_str().unwrap().to_string();
    assert!(Path::new(&item).exists(), "the item is in the bin: {item}");
    let u = json(&["undo", v["journal"].as_str().unwrap()]);
    assert_eq!(u["failed"], 0, "{u}");
    assert_eq!(fs::read(&victim).unwrap(), b"put me in the bin");
    assert!(!Path::new(&item).exists(), "restored out of the bin");
}

#[test]
fn a_large_recycle_is_held_for_a_plan_and_apply_carries_it_out() {
    let t = tmp("plan");
    for i in 0..5 {
        file(&t.0.join(format!("junk/f{i}")), b"x");
    }
    let held = json(&["recycle", s(&t.0.join("junk")), "--plan-files", "3"]);
    assert_eq!(held["held"], true, "{held}");
    assert!(t.0.join("junk/f0").exists(), "nothing touched while held");
    let plan = held["plan"].as_str().unwrap().to_string();
    let applied = json(&["apply", &plan]);
    assert_eq!(applied["failed"], 0, "{applied}");
    assert!(!t.0.join("junk").exists());
    let u = json(&["undo", applied["journal"].as_str().unwrap()]);
    assert_eq!(u["failed"], 0, "{u}");
    assert!(t.0.join("junk/f4").exists());
}

#[test]
fn a_plan_whose_files_changed_is_not_applied_to_them() {
    let t = tmp("stale");
    file(&t.0.join("junk/a"), b"x");
    let held = json(&["recycle", s(&t.0.join("junk")), "--plan"]);
    file(&t.0.join("junk/b"), b"new since the plan");
    let (rc, out, _) = run(&["apply", held["plan"].as_str().unwrap()]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("changed since the plan"), "{out}");
    assert!(t.0.join("junk/b").exists());
}

#[cfg(windows)]
#[test]
fn a_path_too_long_for_the_bin_is_refused_before_anything_is_touched() {
    let t = tmp("long");
    let mut deep = t.0.join("long");
    while deep.as_os_str().len() < 280 {
        deep = deep.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }
    let wide = format!(r"\\?\{}", deep.display());
    fs::create_dir_all(&wide).unwrap();
    fs::write(format!(r"{wide}\f.txt"), b"x").unwrap();
    let (rc, out, _) = run(&["recycle", s(&t.0.join("long"))]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("REFUSED, nothing touched") && out.contains("259"), "{out}");
    assert!(t.0.join("long").exists());
    let _ = fs::remove_dir_all(format!(r"\\?\{}", t.0.display()));
}

#[test]
fn dupes_empty_reparse_free_hash_from_the_command_line() {
    let t = tmp("misc");
    file(&t.0.join("a/x.bin"), &vec![5u8; 2048]);
    file(&t.0.join("b/y.bin"), &vec![5u8; 2048]);
    fs::create_dir_all(t.0.join("void/deeper")).unwrap();
    let d = json(&["dupes", s(&t.0)]);
    assert_eq!(d["groups"].as_array().unwrap().len(), 1);
    assert_eq!(d["wasted_bytes"], 2048);
    let e = json(&["empty", s(&t.0)]);
    assert_eq!(e["empty_dirs"].as_array().unwrap().len(), 2);
    let h = json(&["hash", s(&t.0.join("a/x.bin"))]);
    assert_eq!(h[0]["sha256"].as_str().unwrap().len(), 64);
    let f = json(&["free"]);
    assert!(!f.as_array().unwrap().is_empty());
    let r = json(&["reparse", s(&t.0)]);
    assert!(r.as_array().unwrap().is_empty());
}

#[test]
fn regular_expressions_for_names_and_inside_files() {
    let t = tmp("re");
    file(&t.0.join("001-fix.patch"), b"+ pref(\"a\", 1);\n- old\n");
    file(&t.0.join("readme.patch.orig"), b"x\n");
    file(&t.0.join("v2-thing.patch"), b"nothing\n");
    let (_, out, _) = run(&["find", s(&t.0), "--regex", r"^\d{3}-.*\.patch$", "--paths"]);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains("001-fix.patch"));
    let (_, out, _) = run(&["find", s(&t.0), "--ext", "patch", "--grep-re", r#"^\+ pref\("\w+""#]);
    assert!(out.contains("1: + pref(\"a\", 1);"), "{out}");
    let (rc, _, err) = run(&["find", s(&t.0), "--regex", "(unclosed"]);
    assert_eq!(rc, 2);
    assert!(err.contains("not a regular expression"), "{err}");
}

#[test]
fn reports_and_plans_saved_with_a_byte_order_mark_are_read() {
    let t = tmp("bom");
    file(&t.0.join("tree/a"), b"12345");
    let (_, out, _) = run(&[s(&t.0.join("tree")), "--json"]);
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(out.as_bytes());
    fs::write(t.0.join("old.json"), &bom).unwrap();
    fs::write(t.0.join("new.json"), &bom).unwrap();
    let (rc, _, err) = run(&["compare", s(&t.0.join("old.json")), s(&t.0.join("new.json"))]);
    assert_eq!(rc, 0, "{err}");
    let held = json(&["recycle", s(&t.0.join("tree")), "--plan"]);
    let plan = held["plan"].as_str().unwrap().to_string();
    let mut p = vec![0xEF, 0xBB, 0xBF];
    p.extend(fs::read(&plan).unwrap());
    fs::write(&plan, p).unwrap();
    let applied = json(&["apply", &plan]);
    assert_eq!(applied["failed"], 0, "{applied}");
    let u = json(&["undo", applied["journal"].as_str().unwrap()]);
    assert_eq!(u["failed"], 0, "{u}");
}

#[test]
fn a_hard_linked_file_is_one_file_when_links_are_checked() {
    let t = tmp("hl");
    file(&t.0.join("a/blob"), &vec![1u8; 1000]);
    fs::create_dir_all(t.0.join("b")).unwrap();
    fs::hard_link(t.0.join("a/blob"), t.0.join("b/blob")).unwrap();
    let mut args = vec![s(&t.0)];
    if cfg!(windows) {
        args.push("--links");
    }
    let v = json(&args);
    assert_eq!(v["roots"][0]["totals"]["files"], 1, "{}", v["roots"][0]["totals"]);
    assert_eq!(v["roots"][0]["totals"]["hardlink_repeats"], 1);
}

#[test]
fn newest_and_oldest() {
    let t = tmp("age");
    file(&t.0.join("old"), b"1");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    file(&t.0.join("new"), b"2");
    let (_, out, _) = run(&["newest", s(&t.0), "-n", "1"]);
    assert!(out.trim_end().ends_with("new"), "{out}");
    let (_, out, _) = run(&["oldest", s(&t.0), "-n", "1"]);
    assert!(out.trim_end().ends_with("old"), "{out}");
}
