// SPDX-License-Identifier: AGPL-3.0-or-later
//! gorilla-size - Gorilla Size Calculation.
//!
//! How big folders are, where the space went, what to do about it. Every core, measured per disk; never a permanent
//! delete; never the network. Born 2026-10-16: `du` ran 25 minutes on one Firefox tree of 480,000 files and was
//! stopped, a hand-typed robocopy line got the numbers, and a "Recycle Bin" call destroyed a folder instead.


use gorilla_size::{actions, compare, content, extra, glob, mft, output, profile, units, walk};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
gorilla-size - Gorilla Size Calculation: how big folders are, where the space went, what to do about it.
Never follows a junction or symlink (each is listed). Never deletes permanently. Never uses the network.

MEASURE
  gorilla-size [OPTIONS] [PATH ...]        biggest first, as a tree (PATH defaults to .)
  gorilla-size types [PATH ...]            bytes and files by extension
  gorilla-size newest|oldest [PATH ...]    files by age (-n N)
  gorilla-size watch PATH --every SECS     re-measure, print the growth (--count N to stop); --alert-growth 1G:
                                           ALERT and exit 3 when one interval grows more than that; honors
                                           --exclude, --gitignore, --no-hidden like a measurement
  gorilla-size free                        every drive: kind, size, free
  gorilla-size compare OLD.json NEW.json   what grew, shrank, appeared or went (--tree: in the shape of the
                                           folders, honoring --ascii; with --json a nested tree)

FIND AND READ
  gorilla-size find [FILTERS] [PATH ...]   every file/folder/link that matches, with its size
  gorilla-size hash PATH ...               SHA-256 of files (folders: every file under them); the CPU's SHA
                                           instructions when it has them (--json: sha256_impl sha-ni|portable)
  gorilla-size dupes [--min-size S] PATH ...   the same bytes in more than one place
  gorilla-size empty PATH                  empty folders and zero-byte files
  gorilla-size reparse PATH                every link: kind, target, broken or not

ACT (verified, journalled, undoable; never a permanent delete)
  gorilla-size copy SRC ... --to DIR       every byte compared after the copy
  gorilla-size move SRC ... --to DIR       same drive: rename; other drive: verified copy + recycle
  gorilla-size recycle PATH ...            to the Recycle Bin / trash; refused when Windows would destroy instead;
                                           verified in the bin's own records
  gorilla-size undo JOURNAL                reverse what a journal records
  gorilla-size trash [list]                what is in the Recycle Bin / trash: original place, size, date
  gorilla-size trash restore NAME|PATH ... put items back (by original path, or by name when it is unique)
  gorilla-size apply PLAN                  carry out a plan written for a large operation
  find ... --copy-to DIR | --move-to DIR | --recycle    the same, on what find matched

MACHINE
  gorilla-size bench A B [--rounds N] -- ARGS   two programs, same ARGS, alternating; a verdict only when the
                                           middle halves of the runs do not overlap, else INCONCLUSIVE
  gorilla-size bench --profile [PATH] [-j N] [--rounds N]   the calibrated thread count (or -j N) against every
                                           core, walking PATH, alternating; median, middle half, min-max, verdict
                                           (both benches: 10 rounds unless --rounds; never fewer than 3)
  gorilla-size profile                     this machine, its SHA-256 path, the calibrated thread count per disk
  gorilla-size profile --recalibrate [SAMPLE] [--rounds N]   measure 1..N threads on SAMPLE, keep the fastest

REPORT OPTIONS
  -d, --depth N        levels shown (default 1)            -n, --number N   entries per level (default 20)
  -f, --files N        also the N largest files anywhere   --types          bytes by extension as well
  --disk               size on disk instead of apparent    --links          Windows: hard links counted once
  --gitignore          skip what .gitignore files ignore   --exclude PAT    skip (gitignore syntax; repeatable)
  --no-hidden          skip hidden entries                 --cross-filesystems  (Unix) enter other mounts
  --reverse            smallest first                      --flat           one list of paths at --depth
  --long               attributes/permissions, date, owner --progress       live count on stderr
  --list-links  --list-errors  --strict (exit 3 when anything was unreadable)
  --mft                (Windows, administrator) read the NTFS master file table: a whole drive in seconds
  -j, --threads N      worker threads (default: the calibrated count for that disk, else every core)
  --json               machine-readable                    --ascii          plain characters

FIND FILTERS AND OUTPUT
  --name GLOB  --regex RE  --ext a,b  --min-size S  --max-size S  --newer AGE  --older AGE  --type f|d|l
  --sort size|name|age   -n N   -i/--ignore-case  -s/--case-sensitive
  --paths  -0/--print0  --jsonl  --format '{path}\\t{bytes}\\t{size}\\t{kind}\\t{date}\\t{name}\\t{sha256}'
  --head N  --tail N  --lines A:B   (show those lines of each matched text file)   --max-bytes N (default 16384:
                the most shown per file AND the most of one line held in memory; longer: truncated at N bytes)
  --grep TEXT   keep only text files with a line containing TEXT, and show those lines (-i folds case)
  --grep-re RE  the same with a regular expression (one of --grep, --grep-re)   --max-hits N (default 20 per file)
  --hash        SHA-256 of every file listed, after all filters (grep included) and -n
  binary files: size, kind (PE/MZ ELF PNG JPEG GIF ZIP GZIP PDF SQLite) and the first 16 bytes, never dumped

LARGE OPERATIONS
  above --plan-files N (default 1000) or --plan-bytes S (default 10G) a move or recycle writes a PLAN and stops
  (exit 4); review it, then `gorilla-size apply PLAN`. --plan always writes the plan. copy is never held.

SIZES 500M 2G 1.5T (binary)   AGES 90s 30m 12h 7d 2w 1y
EXIT 0 done (nothing found), 2 bad usage, 3 found (dupes: duplicates, empty: empties, reparse: a broken link,
     watch: --alert-growth) or something failed or was unreadable (--strict), 4 held for a plan.
     With --json an error is {\"error\": \"...\", \"exit\": N} on stdout.
";

#[derive(Default)]
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
    gitignore: bool,
    exclude: Vec<String>,
    no_hidden: bool,
    by_type: bool,
    reverse: bool,
    flat: bool,
    long: bool,
    progress: bool,
    out_paths: bool,
    print0: bool,
    jsonl: bool,
    format: Option<String>,
    head: Option<usize>,
    tail: Option<usize>,
    lines: Option<(usize, usize)>,
    max_bytes: usize,
    grep: Option<String>,
    grep_re: Option<String>,
    regexes: Vec<String>,
    max_hits: usize,
    copy_to: Option<PathBuf>,
    move_to: Option<PathBuf>,
    recycle: bool,
    to: Option<PathBuf>,
    overwrite: bool,
    plan_files: u64,
    plan_bytes: u64,
    plan: bool,
    every: Option<u64>,
    count: Option<u64>,
    recalibrate: bool,
    rounds: Option<usize>,
    bench_profile: bool,
    alert_growth: Option<u64>,
    tree: bool,
    hash: bool,
}

const MODES: [&str; 20] = ["find", "compare", "types", "newest", "oldest", "watch", "free", "hash", "dupes", "empty",
                           "reparse", "copy", "move", "recycle", "undo", "apply", "profile", "report", "bench",
                           "trash"];

fn parse(argv: Vec<String>) -> Result<Args, String> {
    let mut a = Args { mode: "report".into(), depth: 1, sort: "size".into(), max_bytes: 16384, max_hits: 20,
                       plan_files: 1000, plan_bytes: 10 << 30,
                       ignore_case: cfg!(any(windows, target_os = "macos")), ..Default::default() };
    let mut it = argv.into_iter().peekable();
    if let Some(first) = it.peek() {
        if MODES.contains(&first.as_str()) {
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
            "--gitignore" => a.gitignore = true,
            "--exclude" => a.exclude.push(val("--exclude")?),
            "--no-hidden" => a.no_hidden = true,
            "--types" => a.by_type = true,
            "--reverse" | "-r" => a.reverse = true,
            "--flat" => a.flat = true,
            "--long" | "-l" => a.long = true,
            "--progress" => a.progress = true,
            "--paths" => a.out_paths = true,
            "-0" | "--print0" => a.print0 = true,
            "--jsonl" => a.jsonl = true,
            "--format" => a.format = Some(val("--format")?),
            "--head" => a.head = Some(num(val("--head")?, "--head")?),
            "--tail" => a.tail = Some(num(val("--tail")?, "--tail")?),
            "--lines" => {
                let v = val("--lines")?;
                let (x, y) = v.split_once(':').ok_or("--lines A:B")?;
                a.lines = Some((num(x.into(), "--lines")?, num(y.into(), "--lines")?));
            }
            "--max-bytes" => a.max_bytes = units::parse_size(&val("--max-bytes")?)? as usize,
            "--grep" => a.grep = Some(val("--grep")?),
            "--grep-re" => a.grep_re = Some(val("--grep-re")?),
            "--regex" => a.regexes.push(val("--regex")?),
            "--max-hits" => a.max_hits = num(val("--max-hits")?, "--max-hits")?,
            "--copy-to" => a.copy_to = Some(PathBuf::from(val("--copy-to")?)),
            "--move-to" => a.move_to = Some(PathBuf::from(val("--move-to")?)),
            "--recycle" => a.recycle = true,
            "--to" => a.to = Some(PathBuf::from(val("--to")?)),
            "--overwrite" => a.overwrite = true,
            "--plan-files" => a.plan_files = num(val("--plan-files")?, "--plan-files")? as u64,
            "--plan-bytes" => a.plan_bytes = units::parse_size(&val("--plan-bytes")?)?,
            "--plan" => a.plan = true,
            "--every" => a.every = Some(num(val("--every")?, "--every")? as u64),
            "--count" => a.count = Some(num(val("--count")?, "--count")? as u64),
            "--recalibrate" => a.recalibrate = true,
            "--rounds" => a.rounds = Some(num(val("--rounds")?, "--rounds")?.max(1)),
            "--profile" => a.bench_profile = true,
            "--tree" => a.tree = true,
            "--hash" => a.hash = true,
            "--alert-growth" => a.alert_growth = Some(units::parse_size(&val("--alert-growth")?)?),
            x => return Err(format!("unknown option {x} (--help lists them)")),
        }
    }
    for r in a.regexes.iter().chain(a.grep_re.iter()) {
        regex::RegexBuilder::new(r).build().map_err(|e| format!("not a regular expression: {r}: {e}"))?;
    }
    if a.grep.is_some() && a.grep_re.is_some() {
        // one selector: with both, which files "match" would be a guess
        return Err("choose one of --grep, --grep-re".into());
    }
    let acts = [a.copy_to.is_some(), a.move_to.is_some(), a.recycle].iter().filter(|x| **x).count();
    if acts > 1 {
        return Err("choose one of --copy-to, --move-to, --recycle".into());
    }
    Ok(a)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    // bench: everything after `--` belongs to the two programs, not to gorilla-size
    let mut bench_args = Vec::new();
    if argv.first().map(|s| s.as_str()) == Some("bench") {
        if let Some(i) = argv.iter().position(|x| x == "--") {
            bench_args = argv.split_off(i + 1);
            argv.pop();
        }
    }
    // a usage error comes before Args exist: --json anywhere on the line still gets its answer as JSON
    let wants_json = argv.iter().any(|x| x == "--json");
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
        Err(e) => return ExitCode::from(fail_as(wants_json, 2, &format!("gorilla-size: {e}"))),
    };
    let first = a.paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
    let threads = a.threads.or_else(|| profile::threads_for(&absolute(&first)));
    if let Some(n) = threads {
        let _ = rayon::ThreadPoolBuilder::new().num_threads(n.max(1)).build_global();
    }
    let threads_from = if a.threads.is_some() { "asked" } else if threads.is_some() { "profile" } else { "every core" };
    let code = match a.mode.as_str() {
        "compare" => run_compare(&a),
        "free" => run_free(&a),
        "hash" => run_hash(&a),
        "dupes" => run_dupes(&a),
        "empty" => run_empty(&a),
        "reparse" => run_reparse(&a),
        "copy" | "move" | "recycle" => run_act_paths(&a),
        "undo" => run_undo(&a),
        "apply" => run_apply(&a),
        "profile" => run_profile(&a),
        "watch" => run_watch(&a),
        "bench" => run_bench(&a, &bench_args),
        "trash" => run_trash(&a),
        _ => run_measure(&a, threads_from),
    };
    ExitCode::from(code)
}

fn need_paths(a: &Args, what: &str) -> Option<u8> {
    if a.paths.is_empty() {
        return Some(fail(a, 2, what));
    }
    None
}

/// The first PATH that does not exist, as an error (exit 2): a mistyped path must not look like a clean result.
fn missing(a: &Args) -> Option<u8> {
    a.paths.iter().map(|p| absolute(p)).find(|p| std::fs::symlink_metadata(p).is_err())
        .map(|p| fail(a, 2, format!("{} does not exist", p.display())))
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

/// An error the caller must see, and its exit code. Under --json it is one JSON object on STDOUT,
/// {"error": "...", "exit": n}, so a program that parses stdout gets an answer it can read instead of an empty
/// stream and a sentence on stderr (plan section 1: the error contract). Otherwise one line on stderr.
fn fail(a: &Args, code: u8, msg: impl std::fmt::Display) -> u8 {
    fail_as(a.json, code, &format!("gorilla-size {}: {msg}", a.mode))
}

fn fail_as(json: bool, code: u8, msg: &str) -> u8 {
    if json {
        println!("{}", serde_json::to_string_pretty(&json!({"error": msg, "exit": code})).unwrap());
    } else {
        eprintln!("{msg}");
    }
    code
}

// ----------------------------------------------------------------------------------------------------- measure

fn make_filter(a: &Args, t: u64) -> walk::Filter {
    let mut kinds = a.types.clone();
    if kinds.is_empty() && (a.mode == "newest" || a.mode == "oldest") {
        kinds.push(walk::Kind::File);
    }
    walk::Filter {
        names: a.names.iter().map(|n| glob::Glob::new(n, a.ignore_case)).collect(),
        regexes: a.regexes.iter().map(|r| re(r, a.ignore_case)).collect(),
        exts: a.exts.clone(),
        min: a.min,
        max: a.max,
        newer: a.newer.map(|d| t.saturating_sub(d.as_secs())),
        older: a.older.map(|d| t.saturating_sub(d.as_secs())),
        kinds,
    }
}

/// checked in parse(), so it compiles here
fn re(r: &str, ignore_case: bool) -> regex::Regex {
    regex::RegexBuilder::new(r).case_insensitive(ignore_case).build().expect("checked in parse")
}

fn opts(a: &Args, finding: bool) -> walk::Opts {
    use std::io::IsTerminal;
    walk::Opts {
        keep_depth: if finding { 0 } else { a.depth.max(1) },
        top_files: a.files,
        disk: a.disk,
        hardlinks_windows: a.links,
        one_filesystem: !a.cross,
        filter: finding.then(|| make_filter(a, now())),
        gitignore: a.gitignore,
        exclude: a.exclude.clone(),
        hidden: !a.no_hidden,
        types: a.by_type || a.mode == "types",
        ignore_case: a.ignore_case,
        progress: a.progress && std::io::stderr().is_terminal(),
    }
}

fn run_measure(a: &Args, threads_from: &str) -> u8 {
    use std::io::IsTerminal;
    let finding = matches!(a.mode.as_str(), "find" | "newest" | "oldest");
    let paths = if a.paths.is_empty() { vec![PathBuf::from(".")] } else { a.paths.clone() };
    let ascii = a.ascii || !std::io::stdout().is_terminal();
    let hardlinks_checked = cfg!(not(windows)) || a.links || a.mft;
    let mut roots_json = Vec::new();
    let mut out = String::new();
    let mut any_errors = false;
    let mut all_found = Vec::new();
    let (mut grep_dropped, mut hashed, mut hash_failed) = (0usize, 0usize, 0usize);
    let started = Instant::now();
    for p in &paths {
        let abs = absolute(p);
        if std::fs::symlink_metadata(&abs).is_err() {
            return fail(a, 2, format!("{} does not exist", abs.display()));
        }
        let t0 = Instant::now();
        let rep = if a.mft {
            match mft::run(&abs, a.depth.max(1), a.files) {
                Ok(r) => r,
                Err(e) => return fail(a, 2, format!("--mft: {e}")),
            }
        } else {
            walk::run(&abs, opts(a, finding))
        };
        let mut rep = rep;
        // find: --grep/--grep-re select first, then sort and -n, then --hash: nothing grep drops (or -n cuts) is
        // ever hashed
        let found = if finding {
            let (kept, dropped) = grep_select(std::mem::take(&mut rep.found), a);
            grep_dropped += dropped;
            kept
        } else {
            Vec::new()
        };
        let secs = t0.elapsed().as_secs_f64();
        any_errors |= !rep.errors.is_empty();
        if finding {
            all_found.extend(found.iter().cloned());
        }
        if a.json && !finding {
            roots_json.push(root_json(&rep, &abs, secs, a));
        } else if a.json {
            let mut r = root_json(&rep, &abs, secs, a);
            let mut sel = sorted_found(found, a);
            let acting = a.copy_to.is_some() || a.move_to.is_some() || a.recycle;
            if a.hash && !acting {
                let (ok, bad) = hash_found(&mut sel);
                hashed += ok;
                hash_failed += bad;
            }
            r["found"] = output::found_json(&sel);
            roots_json.push(r);
        } else if !finding {
            let how = if a.mft { "NTFS master file table" } else if a.disk { "size on disk" } else { "apparent size" };
            out.push_str(&format!("gorilla-size {VERSION}  {}  ({:.2} s, {} threads: {threads_from}, {how})\n",
                                  abs.display(), secs, rayon::current_num_threads()));
            if a.mode != "types" {
                output::text(&rep, &output::Style { ascii, per_level: a.number.unwrap_or(20), depth: a.depth,
                                                    disk: a.disk, width: 20, reverse: a.reverse, long: a.long,
                                                    flat: a.flat }, &mut out);
            }
            if a.by_type || a.mode == "types" {
                output::types_text(&rep, a.number.unwrap_or(20), &mut out);
            }
            output::notes(&rep, a.disk || a.mft, hardlinks_checked, &mut out);
            if a.list_links && !rep.links.is_empty() {
                output::skipped_list("Links not followed", &rep.links, &mut out);
            }
            if a.list_errors && !rep.errors.is_empty() {
                output::skipped_list("Unreadable", &rep.errors, &mut out);
            }
            out.push('\n');
        } else if a.list_errors && !rep.errors.is_empty() {
            output::skipped_list("Unreadable", &rep.errors, &mut out);
        }
    }
    if finding && (a.copy_to.is_some() || a.move_to.is_some() || a.recycle) {
        let sel = sorted_found(all_found, a);
        let items: Vec<PathBuf> = outermost(sel.iter().map(|f| PathBuf::from(&f.path)).collect());
        let op = if a.copy_to.is_some() { "copy" } else if a.move_to.is_some() { "move" } else { "recycle" };
        let to = a.copy_to.clone().or_else(|| a.move_to.clone());
        return act(op, &items, to.as_deref(), a);
    }
    if a.json {
        let mut v = json!({"tool": "gorilla-size", "version": VERSION, "mode": a.mode,
                           "engine": if a.mft { "mft" } else { "walk" },
                           "size": if a.disk { "disk" } else { "apparent" },
                           "hardlinks_counted_once": hardlinks_checked, "threads": rayon::current_num_threads(),
                           "threads_from": threads_from,
                           "seconds": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                           "roots": roots_json});
        // evidence of the order of work: what grep dropped was never hashed (hashed counts only what is listed)
        if a.grep.is_some() || a.grep_re.is_some() {
            v["grep_dropped"] = json!(grep_dropped);
        }
        if a.hash {
            v["hashed"] = json!(hashed);
            v["hash_failed"] = json!(hash_failed);
            v["sha256_impl"] = json!(gorilla_size::sha256::implementation());
        }
        print_json(&v);
    } else if finding {
        let mut sel = sorted_found(all_found, a);
        if a.hash {
            hash_failed += hash_found(&mut sel).1;
        }
        emit_found(&sel, a, &mut out);
        print!("{out}");
    } else {
        print!("{out}");
    }
    if (a.strict && any_errors) || hash_failed > 0 { 3 } else { 0 }
}

fn root_json(rep: &walk::Report, abs: &Path, secs: f64, a: &Args) -> Value {
    let mut r = json!({
        "path": abs.display().to_string(),
        "seconds": (secs * 1000.0).round() / 1000.0,
        "totals": {"bytes": rep.root.bytes, "disk_bytes": rep.root.disk, "files": rep.root.files,
                   "dirs": rep.root.dirs, "links_not_followed": rep.links.len(),
                   "unreadable": rep.errors.len(), "other_filesystems": rep.other_fs.len(),
                   "excluded": rep.excluded, "hidden_skipped": rep.hidden_skipped,
                   "hardlink_repeats": rep.hardlink_repeats,
                   "hardlink_bytes_not_counted_twice": rep.hardlink_bytes},
        "tree": output::node_json(&rep.root),
        "top_files": rep.top_files.iter().map(|(b, p)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
        "links": output::skipped_json(&rep.links),
        "errors": output::skipped_json(&rep.errors),
        "other_filesystems": output::skipped_json(&rep.other_fs),
    });
    if a.by_type || a.mode == "types" {
        r["types"] = output::types_json(rep);
    }
    r
}

/// Found entries to the next step: text, paths, NUL-separated, a format, JSON lines; then the lines asked for.
fn emit_found(v: &[walk::Found], a: &Args, out: &mut String) {
    use std::io::Write;
    let reading = a.head.is_some() || a.tail.is_some() || a.lines.is_some() || a.grep.is_some() || a.grep_re.is_some();
    let mut stdout = std::io::stdout().lock();
    for f in v {
        if a.print0 {
            let _ = stdout.write_all(f.path.as_bytes());
            let _ = stdout.write_all(&[0]);
            continue;
        }
        let line = if a.jsonl {
            let mut j = output::found_one(f);
            if reading && f.kind == walk::Kind::File {
                j["content"] = read_json(Path::new(&f.path), a);
            }
            let _ = writeln!(stdout, "{j}");
            continue;
        } else if a.out_paths {
            f.path.clone()
        } else if let Some(fmt) = &a.format {
            output::format_found(fmt, f)
        } else {
            let mut s = String::new();
            output::found_text(std::slice::from_ref(f), &mut s);
            s.trim_end().to_string()
        };
        let _ = writeln!(stdout, "{line}");
        if reading && f.kind == walk::Kind::File {
            let _ = write!(stdout, "{}", read_text(Path::new(&f.path), a));
        }
    }
    let _ = stdout.write_all(out.as_bytes());
    out.clear();
}

fn pick(a: &Args) -> Option<content::Pick> {
    if let Some((x, y)) = a.lines {
        Some(content::Pick::Range(x, y))
    } else if let Some(n) = a.tail {
        Some(content::Pick::Tail(n))
    } else {
        a.head.map(content::Pick::Head)
    }
}

/// The search inside one file that --grep or --grep-re asks for (None when neither was given).
fn grep_file(p: &Path, a: &Args, max_hits: usize) -> Option<std::io::Result<content::Grep>> {
    // one line in memory is never more than --max-bytes, whatever the file holds (content.rs)
    if let Some(needle) = &a.grep {
        Some(content::grep(p, needle, a.ignore_case, max_hits, 300, a.max_bytes))
    } else {
        a.grep_re.as_ref().map(|r| content::grep_re(p, &re(r, a.ignore_case), max_hits, 300, a.max_bytes))
    }
}

/// "89 50 4e 47 ..." for a person, "89504e47..." for a program
fn hex_spaced(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn read_text(p: &Path, a: &Args) -> String {
    let mut s = String::new();
    match content::sniff(p) {
        Ok(Some(b)) => {
            s.push_str(&format!("    (binary file: not shown; {}, {}, first {} bytes {})\n", units::human(b.bytes),
                                b.kind.unwrap_or("kind unknown"), b.head.len(), hex_spaced(&b.head)));
            return s;
        }
        Ok(None) => {}
        Err(e) => {
            s.push_str(&format!("    (unreadable: {e})\n"));
            return s;
        }
    }
    match grep_file(p, a, a.max_hits) {
        Some(Ok(g)) => {
            for (n, l) in g.hits {
                s.push_str(&format!("    {n:>6}: {l}\n"));
            }
            if g.long_lines > 0 {
                s.push_str(&format!("    ({} line(s) longer than --max-bytes: truncated at {} bytes, only that much \
                                     of them searched)\n", g.long_lines, a.max_bytes));
            }
        }
        Some(Err(e)) => s.push_str(&format!("    (unreadable: {e})\n")),
        None => {}
    }
    if let Some(pk) = pick(a) {
        match content::lines(p, &pk, a.max_bytes) {
            Ok((ls, cut)) => {
                for (n, l) in ls {
                    if n > 0 { s.push_str(&format!("    {n:>6}| {l}\n")); } else { s.push_str(&format!("          | {l}\n")); }
                }
                if cut {
                    s.push_str(&format!("    (truncated at {} bytes: --max-bytes)\n", a.max_bytes));
                }
            }
            Err(e) => s.push_str(&format!("    (unreadable: {e})\n")),
        }
    }
    s
}

fn read_json(p: &Path, a: &Args) -> Value {
    match content::sniff(p) {
        Ok(Some(b)) => {
            return json!({"binary": true, "bytes": b.bytes, "kind": b.kind,
                          "first_bytes": gorilla_size::sha256::hex(&b.head)});
        }
        Ok(None) => {}
        Err(e) => return json!({"error": e.to_string()}),
    }
    let mut v = json!({});
    match grep_file(p, a, a.max_hits) {
        Some(Ok(g)) => {
            v["grep"] = Value::Array(g.hits.into_iter().map(|(n, l)| json!({"line": n, "text": l})).collect());
            if g.long_lines > 0 {
                v["grep_long_lines"] = json!(g.long_lines);
                v["truncated_at"] = json!(a.max_bytes);
            }
        }
        Some(Err(e)) => v["grep"] = json!({"error": e.to_string()}),
        None => {}
    }
    if let Some(pk) = pick(a) {
        v["lines"] = match content::lines(p, &pk, a.max_bytes) {
            Ok((ls, cut)) => {
                let mut l = json!({"cut": cut, "lines": ls.into_iter().map(|(n, l)| json!({"line": n, "text": l}))
                    .collect::<Vec<_>>()});
                if cut {
                    l["truncated_at"] = json!(a.max_bytes);
                }
                l
            }
            Err(e) => json!({"error": e.to_string()}),
        };
    }
    v
}
/// find --grep / --grep-re select: only text files with at least one matching line stay. Folders, links and
/// binary files have no lines to match and are dropped too. Every core. Returns (kept, how many were dropped).
fn grep_select(v: Vec<walk::Found>, a: &Args) -> (Vec<walk::Found>, usize) {
    use rayon::prelude::*;
    if a.grep.is_none() && a.grep_re.is_none() {
        return (v, 0);
    }
    let before = v.len();
    let kept: Vec<walk::Found> = v.into_par_iter().filter(|f| {
        let p = Path::new(&f.path);
        f.kind == walk::Kind::File && matches!(content::sniff(p), Ok(None))
            && matches!(grep_file(p, a, 1), Some(Ok(g)) if !g.hits.is_empty())
    }).collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

/// find --hash: SHA-256 of every file in `v` (already filtered, sorted and cut to -n), every core. Folders and
/// links get none. Returns (hashed, failed); a file that could not be read keeps no hash and makes the exit 3.
fn hash_found(v: &mut [walk::Found]) -> (usize, usize) {
    use rayon::prelude::*;
    let r: Vec<Option<bool>> = v.par_iter_mut().map(|f| {
        if f.kind != walk::Kind::File {
            return None;
        }
        match gorilla_size::sha256::file(Path::new(&f.path), None) {
            Ok(d) => {
                f.sha256 = Some(gorilla_size::sha256::hex(&d));
                Some(true)
            }
            Err(_) => Some(false),
        }
    }).collect();
    (r.iter().filter(|x| **x == Some(true)).count(), r.iter().filter(|x| **x == Some(false)).count())
}

fn sorted_found(mut v: Vec<walk::Found>, a: &Args) -> Vec<walk::Found> {
    let sort = if a.mode == "newest" || a.mode == "oldest" { "age" } else { a.sort.as_str() };
    match sort {
        "name" => v.sort_by(|x, y| x.path.cmp(&y.path)),
        "age" => v.sort_by(|x, y| x.mtime.cmp(&y.mtime).then_with(|| x.path.cmp(&y.path))),
        _ => v.sort_by(|x, y| y.bytes.cmp(&x.bytes).then_with(|| x.path.cmp(&y.path))),
    }
    if a.mode == "newest" {
        v.reverse();
    }
    let default_n = if a.mode == "newest" || a.mode == "oldest" { Some(20) } else { None };
    if let Some(n) = a.number.or(default_n) {
        v.truncate(n);
    }
    v
}

/// A folder and things inside it both selected: only the folder is acted on.
fn outermost(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
    v.sort();
    v.dedup();
    let mut out: Vec<PathBuf> = Vec::new();
    for p in v {
        if !out.iter().any(|o| p.starts_with(o) && &p != o) {
            out.push(p);
        }
    }
    out
}

// --------------------------------------------------------------------------------------------------------- act

fn run_act_paths(a: &Args) -> u8 {
    if let Some(c) = need_paths(a, "give the paths to act on") {
        return c;
    }
    if a.mode != "recycle" && a.to.is_none() {
        return fail(a, 2, "--to DIR is needed");
    }
    let items: Vec<PathBuf> = outermost(a.paths.iter().map(|p| absolute(p)).collect());
    act(&a.mode.clone(), &items, a.to.as_deref(), a)
}

fn act(op: &str, items: &[PathBuf], to: Option<&Path>, a: &Args) -> u8 {
    if items.is_empty() {
        println!("nothing matched: nothing to {op}");
        return 0;
    }
    if let Some(t) = to {
        if !t.is_dir() {
            return fail(a, 2, format!("{} is not a folder", t.display()));
        }
    }
    let (mut files, mut bytes) = (0u64, 0u64);
    for p in items {
        let (n, b) = actions::tally(p);
        files += n;
        bytes += b;
    }
    let big = op != "copy" && (files > a.plan_files || bytes > a.plan_bytes);
    if a.plan || big {
        return write_plan(op, items, to, files, bytes, a, big);
    }
    execute(op, items, to, a)
}

fn write_plan(op: &str, items: &[PathBuf], to: Option<&Path>, files: u64, bytes: u64, a: &Args, big: bool) -> u8 {
    let d = actions::state_dir().join("plans");
    let _ = std::fs::create_dir_all(&d);
    // 2026-10-16: named by the second, two plans written in the same second replaced each other and `apply` carried
    // out a plan nobody had reviewed. Now: milliseconds + process id, and an existing file is never replaced.
    let mut path = PathBuf::new();
    for k in 0..1000 {
        let p = d.join(format!("{}-{}-{k}-{op}.json", actions::stamp(), std::process::id()));
        if std::fs::OpenOptions::new().write(true).create_new(true).open(&p).is_ok() {
            path = p;
            break;
        }
    }
    if path.as_os_str().is_empty() {
        return fail(a, 3, "no free plan name");
    }
    let v = json!({"tool": "gorilla-size", "plan": op, "to": to.map(|t| t.display().to_string()),
                   "overwrite": a.overwrite, "files": files, "bytes": bytes,
                   "items": items.iter().map(|p| {
                       let (n, b) = actions::tally(p);
                       let m = std::fs::symlink_metadata(p).ok().and_then(|m| m.modified().ok())
                           .and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
                       json!({"path": p.display().to_string(), "files": n, "bytes": b, "mtime": m})
                   }).collect::<Vec<_>>()});
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()) {
        return fail(a, 3, format!("could not write the plan: {e}"));
    }
    let why = if big { format!("{} files / {} is over the threshold ({} files / {})", units::count(files),
                               units::human(bytes), units::count(a.plan_files), units::human(a.plan_bytes)) }
              else { "--plan".to_string() };
    if a.json {
        print_json(&json!({"held": true, "why": why, "plan": path.display().to_string(), "op": op,
                           "files": files, "bytes": bytes}));
    } else {
        println!("HELD: {why}. Nothing was touched.\nPlan: {}\nReview it, then: gorilla-size apply \"{}\"",
                 path.display(), path.display());
    }
    4
}

fn execute(op: &str, items: &[PathBuf], to: Option<&Path>, a: &Args) -> u8 {
    let mut j = match actions::Journal::open(op) {
        Ok(j) => j,
        Err(e) => {
            return fail(a, 3, format!("no journal, nothing done: {e}"));
        }
    };
    let mut done = Vec::new();
    for p in items {
        let d = match op {
            "copy" => actions::copy(p, to.unwrap(), a.overwrite, &mut j),
            "move" => actions::mv(p, to.unwrap(), &mut j),
            _ => actions::recycle(p, &mut j),
        };
        let stop = !d.ok && d.why.starts_with("PERMANENTLY");
        done.push(d);
        if stop {
            break;
        }
    }
    report_done(&done, &j.path, a)
}

fn report_done(done: &[actions::Done], journal: &Path, a: &Args) -> u8 {
    let failed = done.iter().filter(|d| !d.ok).count();
    if a.json {
        print_json(&json!({"journal": journal.display().to_string(), "ok": done.len() - failed, "failed": failed,
                           "actions": done.iter().map(|d| json!({"op": d.op, "from": d.from, "to": d.to, "ok": d.ok,
                                                                  "why": d.why})).collect::<Vec<_>>()}));
    } else {
        for d in done {
            println!("{} {:<8} {}{}  ({})", if d.ok { "OK  " } else { "FAIL" }, d.op, d.from,
                     if d.to.is_empty() { String::new() } else { format!(" -> {}", d.to) }, d.why);
        }
        println!("journal: {}  (undo: gorilla-size undo \"{}\")", journal.display(), journal.display());
    }
    if failed > 0 { 3 } else { 0 }
}

fn run_undo(a: &Args) -> u8 {
    if let Some(c) = need_paths(a, "give the journal to undo") {
        return c;
    }
    let done = actions::undo(&a.paths[0]);
    report_done(&done, &a.paths[0], a)
}

fn run_apply(a: &Args) -> u8 {
    if let Some(c) = need_paths(a, "give the plan to apply") {
        return c;
    }
    let text = match std::fs::read_to_string(&a.paths[0]) {
        Ok(t) => t,
        Err(e) => {
            return fail(a, 2, e);
        }
    };
    // a plan reviewed and saved in Notepad or PowerShell 5.1 can start with a UTF-8 byte-order mark
    let v: Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
        Ok(v) => v,
        Err(e) => {
            return fail(a, 2, format!("not a plan: {e}"));
        }
    };
    let op = v["plan"].as_str().unwrap_or("").to_string();
    if !["copy", "move", "recycle"].contains(&op.as_str()) {
        return fail(a, 2, format!("unknown plan operation {op:?}"));
    }
    let to = v["to"].as_str().map(PathBuf::from);
    let mut items = Vec::new();
    let mut changed = Vec::new();
    for it in v["items"].as_array().cloned().unwrap_or_default() {
        let p = PathBuf::from(it["path"].as_str().unwrap_or(""));
        let (n, b) = actions::tally(&p);
        let m = std::fs::symlink_metadata(&p).ok().and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
        if it["files"] == n && it["bytes"] == b && it["mtime"] == json!(m) {
            items.push(p);
        } else {
            changed.push(p.display().to_string());
        }
    }
    for c in &changed {
        println!("SKIP     {c}  (changed since the plan was written: not touched)");
    }
    let mut args = Args { overwrite: v["overwrite"] == true, json: a.json, ..Default::default() };
    args.mode = op.clone();
    let code = execute(&op, &items, to.as_deref(), &args);
    if !changed.is_empty() { 3 } else { code }
}

// ------------------------------------------------------------------------------------------------- read-only

fn run_hash(a: &Args) -> u8 {
    if let Some(c) = need_paths(a, "give files or folders").or_else(|| missing(a)) {
        return c;
    }
    let v = extra::hash(&a.paths.iter().map(|p| absolute(p)).collect::<Vec<_>>());
    let bad = v.iter().filter(|x| x.1.is_err()).count();
    // which block function produced the digests ("sha-ni" or "portable"): a speed claim names its code path.
    // On every entry, so the output stays the array it always was.
    let imp = gorilla_size::sha256::implementation();
    if a.json {
        print_json(&Value::Array(v.iter().map(|(p, h, b)| match h {
            Ok(h) => json!({"path": p, "sha256": h, "bytes": b, "sha256_impl": imp}),
            Err(e) => json!({"path": p, "error": e}),
        }).collect()));
    } else {
        for (p, h, _) in &v {
            match h {
                Ok(h) => println!("{h}  {p}"),
                Err(e) => println!("ERROR  {p}  ({e})"),
            }
        }
    }
    if bad > 0 { 3 } else { 0 }
}

fn run_dupes(a: &Args) -> u8 {
    if let Some(c) = need_paths(a, "give folders to search").or_else(|| missing(a)) {
        return c;
    }
    let g = extra::dupes(&a.paths.iter().map(|p| absolute(p)).collect::<Vec<_>>(), a.min.unwrap_or(1));
    let wasted: u64 = g.iter().map(|x| x.bytes * (x.paths.len() as u64 - 1)).sum();
    if a.json {
        print_json(&json!({"groups": g.iter().map(|x| json!({"bytes": x.bytes, "sha256": x.hash, "paths": x.paths}))
            .collect::<Vec<_>>(), "wasted_bytes": wasted}));
    } else {
        for x in g.iter().take(a.number.unwrap_or(usize::MAX)) {
            println!("{} copies x {} (wasted {})  sha256 {}", x.paths.len(), units::human(x.bytes),
                     units::human(x.bytes * (x.paths.len() as u64 - 1)), &x.hash[..16]);
            for p in &x.paths {
                println!("    {p}");
            }
        }
        println!("{} group(s), {} could be freed by keeping one copy of each", g.len(), units::human(wasted));
    }
    // exit contract (plan section 1): 3 = something was found, so a script can branch without parsing
    if g.is_empty() { 0 } else { 3 }
}

/// The root of empty/reparse (default: here); None (and the error said) when it does not exist. 2026-10-18: a
/// mistyped path printed "0 empty folder(s)" and exit 0, the same as a clean folder.
fn existing_root(a: &Args) -> Result<PathBuf, u8> {
    let root = absolute(a.paths.first().map(|p| p.as_path()).unwrap_or(Path::new(".")));
    if std::fs::symlink_metadata(&root).is_err() {
        return Err(fail(a, 2, format!("{} does not exist", root.display())));
    }
    Ok(root)
}

fn run_empty(a: &Args) -> u8 {
    let root = match existing_root(a) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let (dirs, files) = extra::empty(&root);
    if a.json {
        print_json(&json!({"empty_dirs": dirs, "zero_byte_files": files}));
    } else {
        for d in &dirs {
            println!("dir   {d}");
        }
        for f in &files {
            println!("file  {f}");
        }
        println!("{} empty folder(s), {} zero-byte file(s)", dirs.len(), files.len());
    }
    if dirs.is_empty() && files.is_empty() { 0 } else { 3 }
}

fn run_reparse(a: &Args) -> u8 {
    let root = match existing_root(a) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let v = extra::reparse(&root);
    if a.json {
        print_json(&Value::Array(v.iter().map(|(p, k, t, ok)| json!({"path": p, "kind": k, "target": t,
                                                                         "target_exists": ok})).collect()));
    } else {
        for (p, k, t, ok) in &v {
            println!("{}  {p} -> {t}  ({k})", if *ok { "ok    " } else { "BROKEN" });
        }
        println!("{} link(s), {} broken", v.len(), v.iter().filter(|x| !x.3).count());
    }
    // links that work are not a finding; a broken one is
    if v.iter().any(|x| !x.3) { 3 } else { 0 }
}

fn run_free(a: &Args) -> u8 {
    let v = extra::free();
    if a.json {
        print_json(&Value::Array(v.iter().map(|(m, k, t, f)| json!({"mount": m, "kind": k, "bytes": t, "free": f}))
            .collect()));
    } else {
        for (m, k, t, f) in &v {
            println!("{m:<12} {k:<10} {:>10} total  {:>10} free  {:>5.1}% free", units::human(*t), units::human(*f),
                     *f as f64 / (*t).max(1) as f64 * 100.0);
        }
    }
    0
}

fn run_compare(a: &Args) -> u8 {
    if a.paths.len() != 2 {
        return fail(a, 2, "give two reports, OLD.json NEW.json");
    }
    let (old, new) = (a.paths[0].to_string_lossy(), a.paths[1].to_string_lossy());
    if a.tree {
        use std::io::IsTerminal;
        return match compare::tree_diff(&old, &new) {
            Ok(t) if a.json => {
                print_json(&compare::tree_json(&t));
                0
            }
            Ok(t) => {
                // the same rule as the measurement tree: line-drawing characters only on a terminal
                let ascii = a.ascii || !std::io::stdout().is_terminal();
                print!("{}", compare::tree_text(&t, a.number.unwrap_or(30), ascii));
                0
            }
            Err(e) => fail(a, 2, e),
        };
    }
    match compare::diff(&old, &new) {
        Ok(v) => {
            if a.json {
                print_json(&compare::json(&v));
            } else {
                print!("{}", compare::text(&v, a.number.unwrap_or(30)));
            }
            0
        }
        Err(e) => fail(a, 2, e),
    }
}

fn run_watch(a: &Args) -> u8 {
    if let Some(c) = need_paths(a, "give a folder to watch") {
        return c;
    }
    let every = a.every.unwrap_or(60).max(1);
    let root = absolute(&a.paths[0]);
    if std::fs::symlink_metadata(&root).is_err() {
        return fail(a, 2, format!("{} does not exist", root.display()));
    }
    // The same skip rules as a measurement: 2026-10-18 audit found watch walked with totals_only(), so
    // --exclude, --gitignore and --no-hidden were accepted and silently ignored, and the numbers disagreed with
    // `gorilla-size PATH --exclude ...` on the same folder.
    let walk_opts = || walk::Opts { gitignore: a.gitignore, exclude: a.exclude.clone(), hidden: !a.no_hidden,
                                    ignore_case: a.ignore_case, one_filesystem: !a.cross,
                                    ..walk::Opts::totals_only() };
    let mut last: Option<u64> = None;
    let mut n = 0u64;
    loop {
        let r = walk::run(&root, walk_opts());
        let b = r.root.bytes;
        let delta = last.map(|l| b as i128 - l as i128);
        // --alert-growth: only GROWTH between two consecutive measurements above the threshold; shrinking never
        // alerts. The run stops at the first alert with exit 3, so a script (or a scheduled task at 3 a.m.) wakes up.
        let alert = matches!((delta, a.alert_growth), (Some(d), Some(limit)) if d > limit as i128);
        let stamp = now();
        if a.json {
            let mut j = json!({"time": stamp, "bytes": b, "files": r.root.files, "delta": delta.map(|d| d as i64),
                               "excluded": r.excluded, "hidden_skipped": r.hidden_skipped});
            if alert {
                j["alert"] = json!(true);
                j["alert_growth"] = json!(a.alert_growth);
            }
            println!("{j}");
        } else {
            let d = delta.map(|d| format!("  {}{}", if d >= 0 { "+" } else { "-" }, units::human(d.unsigned_abs() as u64)))
                .unwrap_or_default();
            println!("{}  {:>10}  {} files{d}", output::date(Some(stamp)), units::human(b), units::count(r.root.files));
            if alert {
                println!("ALERT: {} grew by {} in {every} s, more than --alert-growth {}", root.display(),
                         units::human(delta.unwrap_or(0) as u64), units::human(a.alert_growth.unwrap_or(0)));
            }
        }
        if alert {
            return 3;
        }
        last = Some(b);
        n += 1;
        if a.count.is_some_and(|c| n >= c) {
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_secs(every));
    }
}

/// trash [list] | trash restore NAME-OR-ORIGINAL-PATH ...: what is in the Recycle Bin / trash, read from its own
/// records, and putting items back. Emptying it is not here: gorilla-size never deletes for good.
fn run_trash(a: &Args) -> u8 {
    let sub = a.paths.first().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "list".into());
    match sub.as_str() {
        "list" => {
            let all = actions::bin_list();
            let (orphans, v): (Vec<_>, Vec<_>) = all.into_iter().partition(|i| i.orphan());
            let total: u64 = v.iter().map(|i| i.bytes).sum();
            if a.json {
                let one = |i: &actions::BinItem| json!({"original": i.original, "bytes": i.bytes, "deleted": i.deleted,
                                                         "item": i.item.display().to_string()});
                print_json(&json!({"items": v.iter().map(one).collect::<Vec<_>>(), "count": v.len(), "bytes": total,
                                   "orphan_records": orphans.iter().map(one).collect::<Vec<_>>()}));
            } else {
                for i in v.iter().take(a.number.unwrap_or(usize::MAX)) {
                    println!("{:>10}  {}  {}", units::human(i.bytes), output::date(i.deleted), i.original);
                }
                println!("{} item(s), {} in the Recycle Bin", v.len(), units::human(total));
                if !orphans.is_empty() {
                    println!("{} record(s) whose item is gone (Explorer hides them; not counted):", orphans.len());
                    for i in &orphans {
                        println!("            {}  {}", output::date(i.deleted), i.original);
                    }
                }
            }
            0
        }
        "restore" => {
            let wanted: Vec<String> = a.paths[1..].iter().map(|p| p.to_string_lossy().into_owned()).collect();
            if wanted.is_empty() {
                return fail(a, 2, "restore: give the original path (or the name) of what to put back");
            }
            let done = actions::bin_restore(&wanted);
            let failed = done.iter().filter(|d| !d.ok).count();
            if a.json {
                print_json(&json!({"ok": done.len() - failed, "failed": failed, "actions": done.iter().map(|d|
                    json!({"op": d.op, "from": d.from, "to": d.to, "ok": d.ok, "why": d.why})).collect::<Vec<_>>()}));
            } else {
                for d in &done {
                    println!("{} {} -> {}  ({})", if d.ok { "OK  " } else { "FAIL" }, d.from, d.to, d.why);
                }
            }
            if failed > 0 { 3 } else { 0 }
        }
        x => {
            fail(a, 2, format!("{x}? use list or restore"))
        }
    }
}

/// bench A B [--rounds N] -- ARGS...: run A and B with the same ARGS, alternating (A B, B A, ...) after one warm-up
/// each, and give a verdict only when the two spreads do not overlap. 2026-10-16: a hand-run comparison said 12
/// threads were slower than the default, which also used 12: one run is nothing (the confirm rule).
fn run_bench(a: &Args, rest: &[String]) -> u8 {
    if a.bench_profile {
        return run_bench_profile(a);
    }
    if a.paths.len() != 2 {
        return fail(a, 2, "bench: give two programs, then -- and the arguments for both (or --profile PATH)");
    }
    let rounds = bench_rounds(a);
    let run = |exe: &Path| -> Option<f64> {
        let t = Instant::now();
        let ok = std::process::Command::new(exe).args(rest).stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()).status().ok()?.success();
        ok.then(|| t.elapsed().as_secs_f64())
    };
    let (ea, eb) = (&a.paths[0], &a.paths[1]);
    if run(ea).is_none() || run(eb).is_none() {
        return fail(a, 3, "bench: a program failed with those arguments (warm-up)");
    }
    let (mut ta, mut tb) = (Vec::new(), Vec::new());
    for r in 0..rounds {
        let order: [(&PathBuf, bool); 2] = if r % 2 == 0 { [(ea, true), (eb, false)] } else { [(eb, false), (ea, true)] };
        for (exe, is_a) in order {
            match run(exe) {
                Some(s) if is_a => ta.push(s),
                Some(s) => tb.push(s),
                None => return fail(a, 3, format!("bench: {} failed in round {}", exe.display(), r + 1)),
            }
        }
    }
    let (sa, sb) = (Spread::of(&mut ta), Spread::of(&mut tb));
    let verdict = verdict(&sa, &sb, "A", "B");
    if a.json {
        print_json(&json!({"a": ea.display().to_string(), "b": eb.display().to_string(), "args": rest,
                           "rounds": rounds, "a_seconds": ta, "b_seconds": tb, "a_median": sa.median,
                           "b_median": sb.median, "a_spread": sa.json(), "b_spread": sb.json(),
                           "verdict": verdict}));
    } else {
        println!("A {}  {}", ea.display(), sa.text());
        println!("B {}  {}", eb.display(), sb.text());
        println!("{rounds} alternating rounds after a warm-up each. {verdict}");
    }
    0
}

/// Rounds for both benches: 10 unless --rounds says otherwise (plan item 300: 3 rounds gave verdicts that the
/// next 3 did not repeat), never fewer than 3, because a middle half of 2 runs is meaningless.
fn bench_rounds(a: &Args) -> usize {
    a.rounds.unwrap_or(10).max(3)
}

/// The spread of one side of a bench: median, middle half (25th..75th percentile) and the extremes.
struct Spread {
    median: f64,
    q1: f64,
    q3: f64,
    min: f64,
    max: f64,
}

impl Spread {
    /// sorts `v` (the JSON then lists the runs fastest first)
    fn of(v: &mut [f64]) -> Spread {
        v.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let q = |f: f64| v[((v.len() - 1) as f64 * f).round() as usize];
        Spread { median: q(0.5), q1: q(0.25), q3: q(0.75), min: v[0], max: v[v.len() - 1] }
    }

    fn json(&self) -> Value {
        json!({"median": self.median, "q1": self.q1, "q3": self.q3, "min": self.min, "max": self.max})
    }

    fn text(&self) -> String {
        // four decimals: a folder walked in 2 ms shows its spread instead of "0.002-0.002"
        format!("median {:.4} s  (middle half {:.4}-{:.4}, all runs {:.4}-{:.4})", self.median, self.q1, self.q3,
                self.min, self.max)
    }
}

/// A verdict only when the middle halves do not overlap; otherwise INCONCLUSIVE. Medians alone would call a
/// winner on noise (2026-10-16: "12 threads slower than the default", which also used 12).
fn verdict(a: &Spread, b: &Spread, an: &str, bn: &str) -> String {
    if a.q3 < b.q1 {
        format!("{an} FASTER: {:.2}x", b.median / a.median)
    } else if b.q3 < a.q1 {
        format!("{bn} FASTER: {:.2}x", a.median / b.median)
    } else {
        "INCONCLUSIVE: the middle halves of the runs overlap (more --rounds, or a quieter machine)".to_string()
    }
}

/// bench --profile [PATH] [-j N] [--rounds N]: is the calibrated thread count really faster than every core on
/// this folder? The same walk, in this process, with two thread pools, alternating, after a warm-up of each (plan
/// section 3 item 4: "a profile that is not proven is a claim"). -j N names the count to test instead of the
/// profile's. Fails closed: with no calibration for this disk and no -j there is nothing to test, exit 2.
fn run_bench_profile(a: &Args) -> u8 {
    let root = absolute(a.paths.first().map(|p| p.as_path()).unwrap_or(Path::new(".")));
    if std::fs::symlink_metadata(&root).is_err() {
        return fail(a, 2, format!("bench --profile: {} does not exist", root.display()));
    }
    let all = profile::machine().logical;
    let (cand, from) = match a.threads {
        Some(n) => (n.clamp(1, all), "asked (-j)"),
        None => match profile::threads_for(&root) {
            Some(n) => (n.clamp(1, all), "profile"),
            None => return fail(a, 2, format!("bench --profile: no calibrated thread count for the disk of {} \
                                               (run profile --recalibrate there first, or name one with -j N)",
                                              root.display())),
        },
    };
    let rounds = bench_rounds(a);
    let (mut tp, mut ta) = profile::bench_threads(&root, cand, all, rounds);
    let (sp, sa) = (Spread::of(&mut tp), Spread::of(&mut ta));
    let verdict = verdict(&sp, &sa, "PROFILE", "EVERY CORE");
    let same = cand == all;
    if a.json {
        print_json(&json!({"mode": "bench-profile", "path": root.display().to_string(), "rounds": rounds,
                           "profile_threads": cand, "threads_from": from, "all_threads": all,
                           "same_setting": same, "profile_seconds": tp, "all_seconds": ta,
                           "profile": sp.json(), "all": sa.json(), "verdict": verdict}));
    } else {
        println!("profile     {cand:>3} threads ({from})  {}", sp.text());
        println!("every core  {all:>3} threads  {}", sa.text());
        if same {
            println!("(the profile already uses every core: both sides are the same setting)");
        }
        println!("{rounds} alternating rounds after a warm-up each, walking {}. {verdict}", root.display());
    }
    0
}

fn run_profile(a: &Args) -> u8 {
    if a.recalibrate {
        let sample = absolute(a.paths.first().map(|p| p.as_path()).unwrap_or(Path::new(".")));
        let v = profile::calibrate(&sample, a.rounds.unwrap_or(3), |s| eprintln!("  {s}"));
        let (id, _) = profile::volume(&sample);
        if a.json {
            print_json(&v);
        } else {
            let vol = &v["volumes"][id];
            println!("calibrated on {} ({}): best {} threads, median {:.3} s; every core {:.3} s",
                     sample.display(), vol["kind"].as_str().unwrap_or("?"), vol["threads"],
                     vol["median_seconds"].as_f64().unwrap_or(0.0), vol["all_threads_seconds"].as_f64().unwrap_or(0.0));
            println!("medians: {}", vol["medians"]);
            println!("saved: {}", profile::path().display());
        }
        return 0;
    }
    let v = profile::show();
    if a.json {
        print_json(&v);
    } else {
        let m = &v["machine"];
        println!("CPU      {}", m["cpu"].as_str().unwrap_or("?"));
        println!("threads  {} logical; {} performance core(s), {} efficiency core(s)", m["logical"], m["perf_cores"],
                 m["eff_cores"]);
        println!("features {}", m["features"].as_array().map(|f| f.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()
            .join(" ")).unwrap_or_default());
        println!("RAM      {} GiB", m["ram_gib"]);
        println!("SHA-256  {} (hash, dupes, find --hash; chosen at run time)",
                 v["sha256_impl"].as_str().unwrap_or("?"));
        println!("profile  {}{}", v["profile_file"].as_str().unwrap_or(""),
                 if v["saved_is_this_machine"] == true { "" } else { "  (none for this machine yet: profile --recalibrate)" });
        if let Some(vols) = v["saved"]["volumes"].as_object() {
            for (id, x) in vols {
                println!("volume   {id} ({}): {} threads, {:.3} s vs {:.3} s on every core, measured {}",
                         x["kind"].as_str().unwrap_or("?"), x["threads"], x["median_seconds"].as_f64().unwrap_or(0.0),
                         x["all_threads_seconds"].as_f64().unwrap_or(0.0), x["measured"].as_str().unwrap_or("?"));
            }
        }
    }
    0
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
        assert!(p("find --copy-to a --recycle").is_err(), "one action at a time");
        assert_eq!(p("find --lines 3:9").unwrap().lines, Some((3, 9)));
        assert_eq!(p("move a --to b").unwrap().mode, "move");
    }

    #[test]
    fn a_folder_and_its_contents_are_acted_on_once() {
        let v = outermost(vec![PathBuf::from("/a/b/c"), PathBuf::from("/a/b"), PathBuf::from("/a/x"),
                               PathBuf::from("/a/b")]);
        assert_eq!(v, vec![PathBuf::from("/a/b"), PathBuf::from("/a/x")]);
    }
}
