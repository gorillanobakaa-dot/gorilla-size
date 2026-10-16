//! The built binary, run on small trees made for each test: exact byte counts, links never followed, hard links
//! counted once, find filters, compare, the JSON contract and the exit codes.

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
    let p = std::env::temp_dir().join(format!("gorilla-size-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    Tmp(p)
}

fn file(p: &Path, bytes: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![b'x'; bytes]).unwrap();
}

/// a tree of known size: 1000 + 2000 + 3000 + 500 = 6500 bytes, 4 files, 3 folders
fn sample(root: &Path) {
    file(&root.join("a.bin"), 1000);
    file(&root.join("big/b.patch"), 2000);
    file(&root.join("big/deeper/c.patch"), 3000);
    file(&root.join("small/d.txt"), 500);
}

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(BIN).args(args).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

fn json(args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    let (rc, out, err) = run(&a);
    assert_eq!(rc, 0, "{err}");
    serde_json::from_str(&out).unwrap()
}

#[test]
fn exact_totals_and_biggest_first() {
    let t = tmp("totals");
    sample(&t.0);
    let v = json(&[t.0.to_str().unwrap()]);
    let r = &v["roots"][0];
    assert_eq!(r["totals"]["bytes"], 6500);
    assert_eq!(r["totals"]["files"], 4);
    assert_eq!(r["totals"]["dirs"], 3);
    let kids = r["tree"]["children"].as_array().unwrap();
    assert_eq!(kids[0]["name"], "big");
    assert_eq!(kids[0]["bytes"], 5000);
    assert_eq!(kids[1]["name"], "a.bin");
    assert_eq!(kids[2]["bytes"], 500);
    assert_eq!(v["engine"], "walk");
}

#[test]
fn the_text_report_is_plain_when_piped_and_says_what_it_did_not_count() {
    let t = tmp("text");
    sample(&t.0);
    let (rc, out, _) = run(&[t.0.to_str().unwrap(), "-d", "2"]);
    assert_eq!(rc, 0);
    assert!(out.contains("big/"), "{out}");
    assert!(out.contains("deeper/"), "{out}");
    assert!(out.is_ascii(), "piped output must be plain ASCII:\n{out}");
    assert!(out.contains("links not followed"), "{out}");
    assert!(out.contains("unreadable: 0"), "{out}");
}

#[test]
fn per_level_limit_sums_the_rest() {
    let t = tmp("limit");
    for i in 0..5 {
        file(&t.0.join(format!("f{i}")), 100 * (i + 1));
    }
    let (_, out, _) = run(&[t.0.to_str().unwrap(), "-n", "2"]);
    assert!(out.contains("... 3 more"), "{out}");
}

#[test]
fn largest_files_anywhere() {
    let t = tmp("top");
    sample(&t.0);
    let v = json(&[t.0.to_str().unwrap(), "-f", "2"]);
    let top = v["roots"][0]["top_files"].as_array().unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0]["bytes"], 3000);
    assert!(top[0]["path"].as_str().unwrap().ends_with("c.patch"));
    assert_eq!(top[1]["bytes"], 2000);
}

/// 2026-10-16: Gorilla.firefox\src is a junction to the 157 tree; following it counts 4.5 GiB twice.
#[test]
fn a_junction_or_symlink_is_listed_never_followed() {
    let t = tmp("link");
    sample(&t.0);
    let target = t.0.join("big");
    let link = t.0.join("src");
    #[cfg(windows)]
    let made = Command::new("cmd").args(["/C", "mklink", "/J", link.to_str().unwrap(), target.to_str().unwrap()])
        .output().map(|o| o.status.success()).unwrap_or(false);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &link).is_ok();
    assert!(made, "could not make the link for the test");
    let v = json(&[t.0.to_str().unwrap()]);
    let r = &v["roots"][0];
    assert_eq!(r["totals"]["bytes"], 6500, "the linked folder was counted twice");
    assert_eq!(r["totals"]["links_not_followed"], 1);
    assert!(r["links"][0]["path"].as_str().unwrap().ends_with("src"));
    let kinds: Vec<&str> = r["tree"]["children"].as_array().unwrap().iter().map(|c| c["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"link"));
}

#[test]
fn a_hard_linked_file_is_counted_once() {
    let t = tmp("hard");
    file(&t.0.join("one/blob"), 4096);
    fs::create_dir_all(t.0.join("two")).unwrap();
    fs::hard_link(t.0.join("one/blob"), t.0.join("two/blob")).unwrap();
    let mut args = vec![t.0.to_str().unwrap()];
    if cfg!(windows) {
        // Windows: without --links both names are counted, and the report says hard links were NOT checked
        let v = json(&args);
        assert_eq!(v["roots"][0]["totals"]["bytes"], 8192);
        assert_eq!(v["hardlinks_counted_once"], false);
        args.push("--links");
    }
    let v = json(&args);
    assert_eq!(v["roots"][0]["totals"]["bytes"], 4096);
    assert_eq!(v["roots"][0]["totals"]["hardlink_repeats"], 1);
    assert_eq!(v["hardlinks_counted_once"], true);
}

#[test]
fn size_on_disk_is_at_least_whole_clusters() {
    let t = tmp("disk");
    file(&t.0.join("tiny"), 10);
    let v = json(&[t.0.to_str().unwrap(), "--disk"]);
    let r = &v["roots"][0]["totals"];
    assert_eq!(r["bytes"], 10);
    let d = r["disk_bytes"].as_u64().unwrap();
    // NTFS may keep a 10-byte file inside its MFT record (0 clusters); ext4/btrfs allocate a block
    assert!(d == 0 || d >= 512, "disk size {d}");
}

#[test]
fn find_by_name_extension_size_and_type() {
    let t = tmp("find");
    sample(&t.0);
    let found = |extra: &[&str]| -> Vec<String> {
        let mut a = vec!["find", t.0.to_str().unwrap()];
        a.extend_from_slice(extra);
        let v = json(&a);
        v["roots"][0]["found"].as_array().unwrap().iter()
            .map(|f| Path::new(f["path"].as_str().unwrap()).file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(found(&["--name", "*.patch"]), vec!["c.patch", "b.patch"]);
    assert_eq!(found(&["--ext", "txt,bin"]), vec!["a.bin", "d.txt"]);
    assert_eq!(found(&["--type", "d", "--min-size", "4K"]), vec!["big"]);
    assert_eq!(found(&["--type", "f", "--max-size", "600"]), vec!["d.txt"]);
    assert_eq!(found(&["--name", "*.PATCH", "--case-sensitive"]), Vec::<String>::new());
    assert_eq!(found(&["--name", "*.patch", "--older", "1d"]), Vec::<String>::new());
    assert_eq!(found(&["--name", "*.patch", "--newer", "1d"]).len(), 2);
    assert_eq!(found(&["--type", "f", "-n", "1"]), vec!["c.patch"]);
}

#[test]
fn compare_says_what_grew_and_what_is_new() {
    let t = tmp("cmp");
    let tree = t.0.join("tree");
    sample(&tree);
    let before = t.0.join("before.json");
    let (_, out, _) = run(&[tree.to_str().unwrap(), "-d", "3", "--json"]);
    fs::write(&before, out).unwrap();
    file(&tree.join("big/new.bin"), 7000);
    fs::remove_file(tree.join("small/d.txt")).unwrap();
    let after = t.0.join("after.json");
    let (_, out, _) = run(&[tree.to_str().unwrap(), "-d", "3", "--json"]);
    fs::write(&after, out).unwrap();
    let (rc, text, err) = run(&["compare", before.to_str().unwrap(), after.to_str().unwrap()]);
    assert_eq!(rc, 0, "{err}");
    assert!(text.lines().any(|l| l.contains("new") && l.ends_with("new.bin")), "{text}");
    assert!(text.lines().any(|l| l.contains("gone") && l.ends_with("d.txt")), "{text}");
    assert!(text.lines().any(|l| l.contains("grew") && l.ends_with("/big")), "{text}");
}

#[test]
fn bad_usage_and_missing_paths_exit_2() {
    assert_eq!(run(&["--nonsense"]).0, 2);
    assert_eq!(run(&["--depth"]).0, 2);
    assert_eq!(run(&["/this/does/not/exist/anywhere"]).0, 2);
    assert_eq!(run(&["compare", "only-one.json"]).0, 2);
    let (rc, out, _) = run(&["--help"]);
    assert_eq!(rc, 0);
    assert!(out.contains("Never uses the network"));
}

#[test]
fn mft_mode_refuses_plainly_without_administrator_or_off_windows() {
    let t = tmp("mft");
    sample(&t.0);
    let (rc, _out, err) = run(&[t.0.to_str().unwrap(), "--mft"]);
    if rc == 0 {
        return; // running elevated on NTFS: the real mode is tested by the admin test below
    }
    assert_eq!(rc, 2);
    assert!(err.contains("administrator") || err.contains("NTFS"), "{err}");
}

/// Run by hand (elevated): cargo test -- --ignored. The master file table must agree with the walk, file by file.
#[test]
#[ignore]
fn mft_agrees_with_the_walk() {
    let t = tmp("mftreal");
    sample(&t.0);
    let walk = json(&[t.0.to_str().unwrap(), "-d", "3"]);
    let mft = json(&[t.0.to_str().unwrap(), "-d", "3", "--mft"]);
    assert_eq!(mft["engine"], "mft");
    assert_eq!(walk["roots"][0]["totals"]["bytes"], mft["roots"][0]["totals"]["bytes"]);
    assert_eq!(walk["roots"][0]["totals"]["files"], mft["roots"][0]["totals"]["files"]);
}
