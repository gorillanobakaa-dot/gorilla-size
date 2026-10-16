//! gorilla-trash on a MADE-UP bin folder (never the real one): the count must match, read-only files go, a link
//! inside an item is removed as a link and the folder it points to survives.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_gorilla-trash");

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(BIN).args(args).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("gorilla-trash-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

/// One item in a made-up bin, in the system's own format. Returns the item's path.
fn add_item(bin: &Path, id: &str, original: &str) -> PathBuf {
    if cfg!(windows) {
        let wide: Vec<u16> = original.encode_utf16().chain(Some(0)).collect();
        let mut v = Vec::new();
        v.extend(2u64.to_le_bytes());
        v.extend(0u64.to_le_bytes());
        v.extend(0u64.to_le_bytes());
        v.extend((wide.len() as u32).to_le_bytes());
        v.extend(wide.iter().flat_map(|c| c.to_le_bytes()));
        fs::write(bin.join(format!("$I{id}")), v).unwrap();
        bin.join(format!("$R{id}"))
    } else {
        fs::create_dir_all(bin.join("info")).unwrap();
        fs::create_dir_all(bin.join("files")).unwrap();
        fs::write(bin.join("info").join(format!("{id}.trashinfo")),
                  format!("[Trash Info]\nPath={original}\nDeletionDate=2026-10-16T12:00:00\n")).unwrap();
        bin.join("files").join(id)
    }
}

fn link_dir(link: &Path, target: &Path) -> bool {
    #[cfg(windows)]
    {
        Command::new("cmd").args(["/C", "mklink", "/J", link.to_str().unwrap(), target.to_str().unwrap()])
            .output().map(|o| o.status.success()).unwrap_or(false)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

#[test]
fn empties_only_with_the_right_count_and_never_follows_a_link() {
    let root = tmp("empty");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let precious = root.join("precious");
    fs::create_dir_all(&precious).unwrap();
    fs::write(precious.join("keep.txt"), b"must survive").unwrap();

    let a = add_item(&bin, "AAAAAA", r"D:\old\run1");
    for i in 0..300 {
        let d = a.join(format!("d{}", i % 17));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(format!("f{i}.pcapng")), vec![1u8; 100]).unwrap();
    }
    let ro = a.join("readonly.txt");
    fs::write(&ro, b"x").unwrap();
    let mut p = fs::metadata(&ro).unwrap().permissions();
    p.set_readonly(true);
    fs::set_permissions(&ro, p).unwrap();
    assert!(link_dir(&a.join("link-to-precious"), &precious), "could not make the link");
    let b = add_item(&bin, "BBBBBB.zip", r"D:\old\archive.zip");
    fs::write(&b, vec![2u8; 5000]).unwrap();
    add_item(&bin, "CCCCCC", r"D:\gone\long-ago.iso"); // an orphan record: no item

    let (rc, out, _) = run(&["list", "--bin-dir", bin.to_str().unwrap(), "--json"]);
    assert_eq!(rc, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["items"], 2);
    assert_eq!(v["orphan_records"], 1);

    // run() has no terminal on stdin, like an agent: without --confirm it must refuse, not ask
    let (rc, _, err) = run(&["empty", "--bin-dir", bin.to_str().unwrap()]);
    assert_eq!(rc, 4, "no terminal and no --confirm must refuse: {err}");
    assert!(err.contains("no terminal"), "{err}");
    assert!(a.exists() && b.exists(), "nothing deleted on a refusal");
    let (rc, _, err) = run(&["empty", "--confirm", "3", "--bin-dir", bin.to_str().unwrap()]);
    assert_eq!(rc, 4, "wrong count must refuse: {err}");
    assert!(a.exists() && b.exists(), "nothing deleted on a refusal");

    let (rc, out, err) = run(&["empty", "--confirm", "2", "--bin-dir", bin.to_str().unwrap(), "--json"]);
    assert_eq!(rc, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["files"].as_u64().unwrap(), 300 + 1 + 1 + 1, "{v}"); // 300 + read-only + link + the zip
    assert!(!a.exists() && !b.exists(), "the items are gone");
    assert_eq!(fs::read_dir(&bin).unwrap().count() - usize::from(!cfg!(windows)) * 2, 0,
               "records gone too (Linux keeps the empty info/ and files/ folders)");
    assert_eq!(fs::read(precious.join("keep.txt")).unwrap(), b"must survive", "a link was followed");
    let _ = fs::remove_dir_all(&root);
}
