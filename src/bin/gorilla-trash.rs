// SPDX-License-Identifier: AGPL-3.0-or-later
//! gorilla-trash - empty the Recycle Bin (Windows) or the trash (Linux) on every core.
//!
//! This is the ONE program here that deletes for good, so it is a separate file from gorilla-size (which never
//! does):
//!
//!     gorilla-trash empty                     at a terminal: shows the items and asks once (y/N)
//!     gorilla-trash empty --confirm N         without a terminal (agents, scripts): N must equal the items there
//!     gorilla-trash list                      what is in it
//!
//! It deletes exactly the items it showed (or counted), never one that arrived afterwards. 2026-10-16, the owner:
//! a separate `list` then `empty --confirm N` "makes the tool not safer, it only created a second tool call" for a
//! person; the count stays only where nobody can answer a question.
//!
//! Born 2026-10-16: Explorer emptied 1,038,326 items at about 2,700 per second (over 20 minutes), one by one.
//! Here every item, and every folder inside it, is deleted in parallel; read-only files are made writable first;
//! a junction or symlink inside an item is removed as a link, never followed into its target.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Instant;

use gorilla_size::actions::{self, BinItem};
use gorilla_size::fastdir::{self, EKind};
use gorilla_size::{output, units};
use rayon::prelude::*;

const HELP: &str = "\
gorilla-trash - empty the Recycle Bin (Windows) or the trash (Linux) on every core. DELETES FOR GOOD.

  gorilla-trash empty                   at a terminal: shows what is in it and asks once [y/N]
  gorilla-trash empty --confirm N       without a terminal (agents, scripts): N must be the item count there now
  gorilla-trash list                    what is in it and its size
  --json                                machine-readable
  --bin-dir DIR                         one bin folder instead of this account's (for tests on a made-up bin)

Exit 0 done, 2 bad usage, 3 something could not be deleted, 4 refused (the count did not match).
";

struct Tally {
    files: AtomicU64,
    dirs: AtomicU64,
    bytes: AtomicU64,
    errors: Mutex<Vec<String>>,
}

fn writable(p: &Path) {
    if let Ok(md) = std::fs::symlink_metadata(p) {
        let mut perm = md.permissions();
        if perm.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(p, perm);
        }
    }
}

fn remove_file(p: &Path, t: &Tally) {
    let len = std::fs::symlink_metadata(p).map(|m| m.len()).unwrap_or(0);
    let r = std::fs::remove_file(p).or_else(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            writable(p);
            std::fs::remove_file(p)
        } else {
            Err(e)
        }
    });
    match r {
        Ok(()) => {
            t.files.fetch_add(1, Relaxed);
            t.bytes.fetch_add(len, Relaxed);
        }
        Err(e) => t.errors.lock().unwrap().push(format!("{}: {e}", p.display())),
    }
}

/// A link inside the bin is removed as a link: a directory link with remove_dir (the junction itself, not the
/// folder it points to), a file link with remove_file.
fn remove_link(p: &Path, attrs: u32, t: &Tally) {
    let r = if cfg!(windows) && attrs & 0x10 != 0 { std::fs::remove_dir(p) } else { std::fs::remove_file(p) };
    if let Err(e) = r {
        t.errors.lock().unwrap().push(format!("{}: {e}", p.display()));
    } else {
        t.files.fetch_add(1, Relaxed);
    }
}

fn wipe_dir(d: &Path, t: &Tally) {
    let entries = match fastdir::list(d) {
        Ok(v) => v,
        Err(e) => {
            t.errors.lock().unwrap().push(format!("{}: {e}", d.display()));
            return;
        }
    };
    entries.par_iter().for_each(|e| {
        let p = d.join(&e.name);
        match e.kind {
            EKind::Dir => wipe_dir(&p, t),
            EKind::File => remove_file(&p, t),
            EKind::Link(_) => remove_link(&p, e.attrs, t),
        }
    });
    writable(d);
    match std::fs::remove_dir(d) {
        Ok(()) => {
            t.dirs.fetch_add(1, Relaxed);
        }
        Err(e) => t.errors.lock().unwrap().push(format!("{}: {e}", d.display())),
    }
}

fn wipe(i: &BinItem, t: &Tally) {
    match std::fs::symlink_metadata(&i.item) {
        Ok(md) if md.is_dir() && !gorilla_size::os::is_reparse_dir(&md) => wipe_dir(&i.item, t),
        Ok(md) if md.file_type().is_symlink() || gorilla_size::os::is_reparse_dir(&md) => {
            #[cfg(windows)]
            let attrs = { use std::os::windows::fs::MetadataExt; md.file_attributes() };
            #[cfg(not(windows))]
            let attrs = 0;
            remove_link(&i.item, attrs, t)
        }
        Ok(_) => remove_file(&i.item, t),
        Err(_) => {} // an orphan record: only the record goes
    }
    if std::fs::symlink_metadata(&i.item).is_err() {
        let _ = std::fs::remove_file(i.record());
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    if args.is_empty() || flag("-h") || flag("--help") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if flag("-V") || flag("--version") {
        println!("gorilla-trash {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let json = flag("--json");
    let dirs: Vec<PathBuf> = match value("--bin-dir") {
        Some(d) => vec![PathBuf::from(d)],
        None => actions::bin_dirs(),
    };
    let mut items: Vec<BinItem> = dirs.iter().flat_map(|d| actions::bin_list_in(d)).collect();
    items.sort_by(|a, b| b.deleted.cmp(&a.deleted).then_with(|| a.original.cmp(&b.original)));
    let real: Vec<&BinItem> = items.iter().filter(|i| !i.orphan()).collect();
    let bytes: u64 = real.iter().map(|i| i.bytes).sum();
    match args[0].as_str() {
        "list" => {
            if json {
                println!("{}", serde_json::json!({"items": real.len(), "bytes": bytes,
                    "orphan_records": items.len() - real.len(), "confirm_with": real.len()}));
            } else {
                for i in &real {
                    println!("{:>10}  {}  {}", units::human(i.bytes), output::date(i.deleted), i.original);
                }
                println!("{} item(s), {}", real.len(), units::human(bytes));
            }
            ExitCode::SUCCESS
        }
        "empty" => {
            use std::io::{BufRead, IsTerminal, Write};
            if real.is_empty() && items.is_empty() {
                println!("the bin is already empty");
                return ExitCode::SUCCESS;
            }
            match value("--confirm").map(|v| v.parse::<usize>()) {
                Some(Ok(n)) if n == real.len() => {}
                Some(Ok(n)) => {
                    eprintln!("REFUSED: the bin holds {} item(s), not {n}. Nothing deleted.", real.len());
                    return ExitCode::from(4);
                }
                Some(Err(_)) => {
                    eprintln!("gorilla-trash empty: --confirm takes the number of items");
                    return ExitCode::from(2);
                }
                None if std::io::stdin().is_terminal() => {
                    for i in &real {
                        println!("{:>10}  {}  {}", units::human(i.bytes), output::date(i.deleted), i.original);
                    }
                    print!("Delete these {} item(s), {}, FOR GOOD? [y/N] ", real.len(), units::human(bytes));
                    let _ = std::io::stdout().flush();
                    let mut answer = String::new();
                    let _ = std::io::stdin().lock().read_line(&mut answer);
                    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                        println!("nothing deleted");
                        return ExitCode::from(4);
                    }
                }
                None => {
                    eprintln!("REFUSED: no terminal to ask (an agent or a script). Nothing deleted. Give the number of \
                               items you mean to destroy: --confirm {}", real.len());
                    return ExitCode::from(4);
                }
            }
            let t = Tally { files: AtomicU64::new(0), dirs: AtomicU64::new(0), bytes: AtomicU64::new(0),
                            errors: Mutex::new(Vec::new()) };
            let t0 = Instant::now();
            items.par_iter().for_each(|i| wipe(i, &t));
            let secs = t0.elapsed().as_secs_f64();
            let (f, d, b) = (t.files.into_inner(), t.dirs.into_inner(), t.bytes.into_inner());
            let errors = t.errors.into_inner().unwrap();
            let left: Vec<&BinItem> = items.iter().filter(|i| !i.orphan()).collect();
            if json {
                println!("{}", serde_json::json!({"items": real.len(), "files": f, "folders": d, "bytes": b,
                    "seconds": secs, "per_second": ((f + d) as f64 / secs.max(0.001)).round(),
                    "errors": errors, "left": left.iter().map(|i| &i.original).collect::<Vec<_>>()}));
            } else {
                println!("deleted {} item(s): {} files and {} folders, {}, in {:.1} s ({:.0} per second)", real.len(),
                         units::count(f), units::count(d), units::human(b), secs, (f + d) as f64 / secs.max(0.001));
                for e in errors.iter().take(20) {
                    println!("  could not delete: {e}");
                }
                if !left.is_empty() {
                    println!("{} item(s) are still in the bin (see above)", left.len());
                }
            }
            if errors.is_empty() && left.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(3) }
        }
        x => {
            eprintln!("gorilla-trash: {x}? use list or empty (--help)");
            ExitCode::from(2)
        }
    }
}
