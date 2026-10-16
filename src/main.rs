// SPDX-License-Identifier: AGPL-3.0-or-later
//! gorilla-size - Gorilla Size Calculation.
//!
//! How big folders are, where the space went, what can go. Uses every core, reads only, never touches the network.
//! Born 2026-10-16: a model measured four Firefox source trees with `du` (25 minutes on one folder of 480,000 files)
//! and then with an improvised robocopy line, because the harness had no tool for it.

mod compare;
mod glob;
mod mft;
mod os;
mod output;
mod units;
mod walk;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
gorilla-size - Gorilla Size Calculation: how big folders are, where the space went, what can go.
Reads only. Never follows a junction or symlink (each is listed instead). Never uses the network.

USAGE
  gorilla-size [OPTIONS] [PATH ...]        size report, biggest first (PATH defaults to .)
  gorilla-size find [OPTIONS] [PATH ...]   every file or folder that matches, with its size
  gorilla-size compare OLD.json NEW.json   what grew, shrank, appeared or went between two --json reports

REPORT
  -d, --depth N          levels of the tree to show (default 1)
  -n, --number N         entries per level (default 20; the rest are summed in one line)
  -f, --files N          also list the N largest files anywhere under PATH
      --disk             size on disk (allocated, after compression and clusters) instead of apparent size
                         (Linux: free; Windows: one extra call per file)
      --links            count a hard-linked file once (Linux: always on; Windows: opens each file, slower)
      --cross-filesystems  (Linux, macOS) also walk into other mounted file systems (default: stay on one)
      --list-links       list every junction, symlink and mount point that was not followed
      --list-errors      list every entry that could not be read
      --mft              (Windows, administrator) read the volume's NTFS master file table directly instead of
                         walking folders: a whole drive in seconds. Hard links are counted once there for free.
  -j, --threads N        worker threads (default: every core)
      --json             machine-readable report (everything, to --depth)
      --ascii            plain characters (automatic when the output is not a terminal)
      --strict           exit 3 when anything was unreadable

FIND (with: find)
      --name GLOB        name matches (*, ?, [a-z]); repeat for several
      --ext a,b          extensions (files only)
      --min-size S       at least S (500M, 2G, 1.5T); a folder's size is everything under it
      --max-size S
      --newer AGE        changed within AGE (90s, 30m, 12h, 7d, 2w, 1y)
      --older AGE        not changed for AGE
      --type f|d|l       files, folders (dirs) or links; repeat or comma-separate
      --sort size|name|age   (default size)
  -n, --number N         at most N results (default: all)
  -i, --ignore-case / -s, --case-sensitive   (default: ignore case on Windows and macOS)

COMPARE
  -n, --number N         at most N changes (default 30)   --json

EXIT 0 done, 2 bad usage or nothing to measure, 3 --strict and something was unreadable.
";

struct Args {
    mode: String,
    paths: Vec<PathBuf>,
    depth: usize,
    number: Option<usize>,
    files: usize,
    disk: bool,
    links: bool,
    cross: bool,
    list_links: bool,
    list_errors: bool,
    mft: bool,
    threads: Option<usize>,
    json: bool,
    ascii: bool,
    strict: bool,
    names: Vec<String>,
    exts: Vec<String>,
    min: Option<u64>,
    max: Option<u64>,
    newer: Option<std::time::Duration>,
    older: Option<std::time::Duration>,
    types: Vec<walk::Kind>,
    sort: String,
    ignore_case: bool,
}

fn parse(argv: Vec<String>) -> Result<Args, String> {
    let mut a = Args {
        mode: "report".into(), paths: vec![], depth: 1, number: None, files: 0, disk: false, links: false,
        cross: false, list_links: false, list_errors: false, mft: false, threads: None, json: false, ascii: false,
        strict: false, names: vec![], exts: vec![], min: None, max: None, newer: None, older: None, types: vec![],
        sort: "size".into(), ignore_case: cfg!(any(windows, target_os = "macos")),
    };
    let mut it = argv.into_iter().peekable();
    if let Some(first) = it.peek() {
        if first == "find" || first == "compare" {
            a.mode = it.next().unwrap();
        }
    }
    let mut only_paths = false;
    while let Some(arg) = it.next() {
        if only_paths || !arg.starts_with('-') || arg == "-" {
            a.paths.push(PathBuf::from(arg));
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut val = |name: &str| -> Result<String, String> {
            inline.clone().or_else(|| it.next()).ok_or(format!("{name} needs a value"))
        };
        let num = |s: String, name: &str| s.parse::<usize>().map_err(|_| format!("{name}: not a number: {s}"));
        match flag.as_str() {
            "--" => only_paths = true,
            "-h" | "--help" => return Err("HELP".into()),
            "-V" | "--version" => return Err("VERSION".into()),
            "-d" | "--depth" => a.depth = num(val("--depth")?, "--depth")?,
            "-n" | "--number" => a.number = Some(num(val("--number")?, "--number")?),
            "-f" | "--files" => a.files = num(val("--files")?, "--files")?,
            "-j" | "--threads" => a.threads = Some(num(val("--threads")?, "--threads")?),
            "--disk" => a.disk = true,
            "--links" => a.links = true,
            "--cross-filesystems" => a.cross = true,
            "--list-links" => a.list_links = true,
            "--list-errors" => a.list_errors = true,
            "--mft" => a.mft = true,
            "--json" => a.json = true,
            "--ascii" => a.ascii = true,
            "--strict" => a.strict = true,
            "--name" => a.names.push(val("--name")?),
            "--ext" => a.exts.extend(val("--ext")?.split(',').map(|e| e.trim().trim_start_matches('.')
                .to_lowercase()).filter(|e| !e.is_empty())),
            "--min-size" => a.min = Some(units::parse_size(&val("--min-size")?)?),
            "--max-size" => a.max = Some(units::parse_size(&val("--max-size")?)?),
            "--newer" => a.newer = Some(units::parse_age(&val("--newer")?)?),
            "--older" => a.older = Some(units::parse_age(&val("--older")?)?),
            "--type" => {
                for t in val("--type")?.split(',') {
                    a.types.push(match t.trim() {
                        "f" | "file" => walk::Kind::File,
                        "d" | "dir" | "folder" => walk::Kind::Dir,
                        "l" | "link" => walk::Kind::Link,
                        x => return Err(format!("--type: {x}? use f, d or l")),
                    });
                }
            }
            "--sort" => {
                a.sort = val("--sort")?;
                if !["size", "name", "age"].contains(&a.sort.as_str()) {
                    return Err(format!("--sort: {}? use size, name or age", a.sort));
                }
            }
            "-i" | "--ignore-case" => a.ignore_case = true,
            "-s" | "--case-sensitive" => a.ignore_case = false,
            x => return Err(format!("unknown option {x} (--help lists them)")),
        }
    }
    Ok(a)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let a = match parse(argv) {
        Ok(a) => a,
        Err(e) if e == "HELP" => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) if e == "VERSION" => {
            println!("gorilla-size {VERSION}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("gorilla-size: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(n) = a.threads {
        let _ = rayon::ThreadPoolBuilder::new().num_threads(n.max(1)).build_global();
    }
    match a.mode.as_str() {
        "compare" => run_compare(&a),
        _ => run_measure(&a),
    }
}

fn run_compare(a: &Args) -> ExitCode {
    if a.paths.len() != 2 {
        eprintln!("gorilla-size compare: give two reports, OLD.json NEW.json");
        return ExitCode::from(2);
    }
    match compare::diff(&a.paths[0].to_string_lossy(), &a.paths[1].to_string_lossy()) {
        Ok(v) => {
            if a.json {
                println!("{}", serde_json::to_string_pretty(&compare::json(&v)).unwrap());
            } else {
                print!("{}", compare::text(&v, a.number.unwrap_or(30)));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gorilla-size compare: {e}");
            ExitCode::from(2)
        }
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn make_filter(a: &Args, t: u64) -> walk::Filter {
    walk::Filter {
        names: a.names.iter().map(|n| glob::Glob::new(n, a.ignore_case)).collect(),
        exts: a.exts.clone(),
        min: a.min,
        max: a.max,
        newer: a.newer.map(|d| t.saturating_sub(d.as_secs())),
        older: a.older.map(|d| t.saturating_sub(d.as_secs())),
        kinds: a.types.clone(),
    }
}

fn run_measure(a: &Args) -> ExitCode {
    use std::io::IsTerminal;
    let finding = a.mode == "find";
    let paths = if a.paths.is_empty() { vec![PathBuf::from(".")] } else { a.paths.clone() };
    let ascii = a.ascii || !std::io::stdout().is_terminal();
    let t = now();
    let hardlinks_checked = cfg!(not(windows)) || a.links || a.mft;
    let mut roots_json = Vec::new();
    let mut out = String::new();
    let mut any_errors = false;
    let mut all_found = Vec::new();
    let started = Instant::now();
    for p in &paths {
        let abs = absolute(p);
        if !abs.exists() && std::fs::symlink_metadata(&abs).is_err() {
            eprintln!("gorilla-size: {} does not exist", abs.display());
            return ExitCode::from(2);
        }
        let t0 = Instant::now();
        let rep = if a.mft {
            match mft::run(&abs, a.depth.max(1), a.files) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("gorilla-size --mft: {e}");
                    return ExitCode::from(2);
                }
            }
        } else {
            walk::run(&abs, walk::Opts {
                keep_depth: if finding { 0 } else { a.depth.max(1) },
                top_files: a.files,
                disk: a.disk,
                hardlinks_windows: a.links,
                one_filesystem: !a.cross,
                filter: finding.then(|| make_filter(a, t)),
            })
        };
        let secs = t0.elapsed().as_secs_f64();
        any_errors |= !rep.errors.is_empty();
        if a.json {
            let mut r = json!({
                "path": abs.display().to_string(),
                "seconds": (secs * 1000.0).round() / 1000.0,
                "totals": {"bytes": rep.root.bytes, "disk_bytes": rep.root.disk, "files": rep.root.files,
                           "dirs": rep.root.dirs, "links_not_followed": rep.links.len(),
                           "unreadable": rep.errors.len(), "other_filesystems": rep.other_fs.len(),
                           "hardlink_repeats": rep.hardlink_repeats,
                           "hardlink_bytes_not_counted_twice": rep.hardlink_bytes},
                "tree": output::node_json(&rep.root),
                "top_files": rep.top_files.iter().map(|(b, p)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
                "links": output::skipped_json(&rep.links),
                "errors": output::skipped_json(&rep.errors),
                "other_filesystems": output::skipped_json(&rep.other_fs),
            });
            if finding {
                r["found"] = output::found_json(&sorted_found(rep.found.clone(), &a.sort, a.number));
            }
            roots_json.push(r);
        } else if finding {
            all_found.extend(rep.found.clone());
            if a.list_errors && !rep.errors.is_empty() {
                output::skipped_list("Unreadable", &rep.errors, &mut out);
            }
        } else {
            let how = if a.mft { "NTFS master file table" } else if a.disk { "size on disk" } else { "apparent size" };
            out.push_str(&format!("gorilla-size {VERSION}  {}  ({:.2} s, {} threads, {how})\n", abs.display(), secs,
                                  rayon::current_num_threads()));
            output::text(&rep, &output::Style { ascii, per_level: a.number.unwrap_or(20), depth: a.depth,
                                                disk: a.disk, width: 20 }, &mut out);
            output::notes(&rep, a.disk || a.mft, hardlinks_checked, &mut out);
            if a.list_links && !rep.links.is_empty() {
                output::skipped_list("Links not followed", &rep.links, &mut out);
            }
            if a.list_errors && !rep.errors.is_empty() {
                output::skipped_list("Unreadable", &rep.errors, &mut out);
            }
            out.push('\n');
        }
    }
    if a.json {
        let v: Value = json!({"tool": "gorilla-size", "version": VERSION,
                              "mode": if finding { "find" } else { "report" },
                              "engine": if a.mft { "mft" } else { "walk" },
                              "size": if a.disk { "disk" } else { "apparent" },
                              "hardlinks_counted_once": hardlinks_checked, "threads": rayon::current_num_threads(),
                              "seconds": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                              "roots": roots_json});
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else {
        if finding {
            output::found_text(&sorted_found(all_found, &a.sort, a.number), &mut out);
        }
        print!("{out}");
    }
    if a.strict && any_errors {
        return ExitCode::from(3);
    }
    ExitCode::SUCCESS
}

fn sorted_found(mut v: Vec<walk::Found>, sort: &str, limit: Option<usize>) -> Vec<walk::Found> {
    match sort {
        "name" => v.sort_by(|a, b| a.path.cmp(&b.path)),
        "age" => v.sort_by(|a, b| a.mtime.cmp(&b.mtime).then_with(|| a.path.cmp(&b.path))),
        _ => v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path))),
    }
    if let Some(n) = limit {
        v.truncate(n);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Args, String> {
        parse(s.split_whitespace().map(String::from).collect())
    }

    #[test]
    fn arguments() {
        let a = p("find --name *.patch --min-size 2G --type f,d -n 5 C:/x").unwrap();
        assert_eq!(a.mode, "find");
        assert_eq!(a.min, Some(2 << 30));
        assert_eq!(a.types, vec![walk::Kind::File, walk::Kind::Dir]);
        assert_eq!(a.number, Some(5));
        assert_eq!(a.paths, vec![PathBuf::from("C:/x")]);
        assert!(p("--depth").is_err());
        assert!(p("--nonsense").is_err());
        assert!(p("--sort weird").is_err());
        assert_eq!(p("--depth=3").unwrap().depth, 3);
    }
}
