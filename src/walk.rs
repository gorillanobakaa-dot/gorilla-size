//! The walk: every directory read once (fastdir: one kernel batch per directory on Windows, no call per file),
//! sub-directories in parallel on every core (rayon work stealing), nothing followed that would count a tree twice,
//! every error and every link kept so the report can say what it skipped.
//!
//! Memory stays bounded on any size of tree: below `keep_depth` only the totals travel up, not the entries. Matches,
//! links, errors and per-type sums are gathered per directory and handed up with its result: no shared lock per file.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

use rayon::prelude::*;

use crate::fastdir::{self, EKind};
use crate::glob::Glob;
use crate::ignore::Rules;
use crate::os;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File,
    Dir,
    Link,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub kind: Kind,
    /// apparent size (what the files say they hold), hard links counted once
    pub bytes: u64,
    /// size on disk (allocated)
    pub disk: u64,
    pub files: u64,
    pub dirs: u64,
    /// seconds since 1970, of the entry itself
    pub mtime: Option<u64>,
    /// Windows attributes or Unix mode
    pub attrs: u32,
    pub children: Vec<Node>,
}

#[derive(Default)]
pub struct Filter {
    pub names: Vec<Glob>,
    /// --regex: name matches any of these regular expressions
    pub regexes: Vec<regex::Regex>,
    pub exts: Vec<String>,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub newer: Option<u64>,
    pub older: Option<u64>,
    pub kinds: Vec<Kind>,
}

impl Filter {
    pub fn wants(&self, name: &str, kind: Kind, bytes: u64, mtime: Option<u64>) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&kind) {
            return false;
        }
        if !self.names.is_empty() && !self.names.iter().any(|g| g.matches(name)) {
            return false;
        }
        if !self.regexes.is_empty() && !self.regexes.iter().any(|r| r.is_match(name)) {
            return false;
        }
        if !self.exts.is_empty() {
            let e = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
            if kind != Kind::File || !self.exts.contains(&e) {
                return false;
            }
        }
        if self.min.is_some_and(|m| bytes < m) || self.max.is_some_and(|m| bytes > m) {
            return false;
        }
        if let Some(cut) = self.newer {
            if mtime.map_or(true, |t| t < cut) {
                return false;
            }
        }
        if let Some(cut) = self.older {
            if mtime.map_or(true, |t| t > cut) {
                return false;
            }
        }
        true
    }
}

pub struct Opts {
    pub keep_depth: usize,
    pub top_files: usize,
    pub disk: bool,
    pub hardlinks_windows: bool,
    pub one_filesystem: bool,
    pub filter: Option<Filter>,
    /// read .gitignore files on the way down
    pub gitignore: bool,
    /// --exclude patterns (gitignore syntax), relative to the root
    pub exclude: Vec<String>,
    /// include hidden entries (dot-names; Windows hidden attribute)
    pub hidden: bool,
    pub types: bool,
    pub ignore_case: bool,
    pub progress: bool,
}

impl Opts {
    pub fn totals_only() -> Opts {
        Opts { keep_depth: 0, top_files: 0, disk: false, hardlinks_windows: false, one_filesystem: true, filter: None,
               gitignore: false, exclude: vec![], hidden: true, types: false, ignore_case: false, progress: false }
    }
}

#[derive(Debug, Clone)]
pub struct Found {
    pub path: String,
    pub kind: Kind,
    pub bytes: u64,
    pub mtime: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Skipped {
    pub path: String,
    pub why: String,
}

#[derive(Default)]
struct Bag {
    links: Vec<Skipped>,
    errors: Vec<Skipped>,
    other_fs: Vec<Skipped>,
    found: Vec<Found>,
    types: HashMap<String, (u64, u64)>,
    excluded: u64,
    hidden: u64,
}

impl Bag {
    fn absorb(&mut self, o: Bag) {
        self.links.extend(o.links);
        self.errors.extend(o.errors);
        self.other_fs.extend(o.other_fs);
        self.found.extend(o.found);
        for (k, (b, n)) in o.types {
            let e = self.types.entry(k).or_default();
            e.0 += b;
            e.1 += n;
        }
        self.excluded += o.excluded;
        self.hidden += o.hidden;
    }
}

struct Shared<'a> {
    opts: &'a Opts,
    cluster: u64,
    root_dev: Option<u64>,
    top: Mutex<BinaryHeap<Reverse<(u64, String)>>>,
    top_floor: AtomicU64,
    seen: Mutex<HashSet<(u64, u64)>>,
    hardlink_repeats: AtomicU64,
    hardlink_bytes: AtomicU64,
    files_seen: &'a AtomicU64,
}

pub struct Report {
    pub root: Node,
    pub top_files: Vec<(u64, String)>,
    pub links: Vec<Skipped>,
    pub errors: Vec<Skipped>,
    pub other_fs: Vec<Skipped>,
    pub found: Vec<Found>,
    pub hardlink_repeats: u64,
    pub hardlink_bytes: u64,
    /// extension -> (bytes, files); "" = no extension
    pub types: Vec<(String, u64, u64)>,
    pub excluded: u64,
    pub hidden_skipped: u64,
}

impl Shared<'_> {
    fn offer_top(&self, bytes: u64, p: &Path) {
        let k = self.opts.top_files;
        if k == 0 || bytes == 0 {
            return;
        }
        if bytes <= self.top_floor.load(Relaxed) {
            return;
        }
        let mut h = self.top.lock().unwrap();
        h.push(Reverse((bytes, p.display().to_string())));
        if h.len() > k {
            h.pop();
        }
        if h.len() >= k {
            if let Some(Reverse((m, _))) = h.peek() {
                self.top_floor.store(*m, Relaxed);
            }
        }
    }

    fn repeat(&self, p: &Path, e: &fastdir::Entry) -> bool {
        let id = if cfg!(windows) {
            if !self.opts.hardlinks_windows {
                return false;
            }
            std::fs::symlink_metadata(p).ok().and_then(|m| os::hardlink_id(p, &m, true))
        } else {
            e.hardlink
        };
        match id {
            Some(id) => !self.seen.lock().unwrap().insert(id),
            None => false,
        }
    }
}

fn hidden(e: &fastdir::Entry) -> bool {
    e.name.starts_with('.') || (cfg!(windows) && e.attrs & 0x2 != 0)
}

/// Walk `root` (a directory, or a single file) and everything under it.
pub fn run(root: &Path, opts: Opts) -> Report {
    let files_seen = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let rep = std::thread::scope(|s| {
        if opts.progress {
            s.spawn(|| {
                use std::io::Write;
                let mut last = 0;
                while !stop.load(Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let n = files_seen.load(Relaxed);
                    if n != last {
                        eprint!("\r  {} entries read ...", crate::units::count(n));
                        let _ = std::io::stderr().flush();
                        last = n;
                    }
                }
                if last > 0 {
                    eprint!("\r{:40}\r", "");
                }
            });
        }
        let r = run_inner(root, &opts, &files_seen);
        stop.store(true, Relaxed);
        r
    });
    rep
}

fn run_inner(root: &Path, opts: &Opts, files_seen: &AtomicU64) -> Report {
    let cluster = os::cluster_size(root);
    let md = std::fs::symlink_metadata(root);
    let root_dev = md.as_ref().ok().and_then(os::device);
    let sh = Shared {
        opts,
        cluster,
        root_dev,
        top: Mutex::new(BinaryHeap::new()),
        top_floor: AtomicU64::new(0),
        seen: Mutex::new(HashSet::new()),
        hardlink_repeats: AtomicU64::new(0),
        hardlink_bytes: AtomicU64::new(0),
        files_seen,
    };
    let mut rules = Rules::default();
    for x in &opts.exclude {
        rules.add_lines("", x, opts.ignore_case);
    }
    let name = root.display().to_string();
    let mtime = md.as_ref().ok().and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
    let (node, bag) = match md {
        Err(e) => {
            let mut b = Bag::default();
            b.errors.push(Skipped { path: name.clone(), why: e.to_string() });
            (Node { name, kind: Kind::Dir, bytes: 0, disk: 0, files: 0, dirs: 0, mtime: None, attrs: 0,
                    children: vec![] }, b)
        }
        Ok(m) if m.is_dir() => walk(root, String::new(), name, mtime, 0, &rules, &sh),
        Ok(m) => {
            let disk = os::disk_size(root, &m, sh.cluster, sh.opts.disk);
            (Node { name, kind: Kind::File, bytes: m.len(), disk, files: 1, dirs: 0, mtime, attrs: 0,
                    children: vec![] }, Bag::default())
        }
    };
    let mut top: Vec<(u64, String)> = sh.top.into_inner().unwrap().into_iter().map(|Reverse(x)| x).collect();
    top.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let sorted = |mut v: Vec<Skipped>| {
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    };
    let mut types: Vec<(String, u64, u64)> = bag.types.into_iter().map(|(k, (b, n))| (k, b, n)).collect();
    types.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Report {
        root: node,
        top_files: top,
        links: sorted(bag.links),
        errors: sorted(bag.errors),
        other_fs: sorted(bag.other_fs),
        found: bag.found,
        hardlink_repeats: sh.hardlink_repeats.into_inner(),
        hardlink_bytes: sh.hardlink_bytes.into_inner(),
        types,
        excluded: bag.excluded,
        hidden_skipped: bag.hidden,
    }
}

fn walk(dir: &Path, rel: String, name: String, mtime: Option<u64>, depth: usize, rules: &Rules, sh: &Shared)
        -> (Node, Bag) {
    let mut node = Node { name, kind: Kind::Dir, bytes: 0, disk: 0, files: 0, dirs: 0, mtime, attrs: 0,
                          children: vec![] };
    let mut bag = Bag::default();
    let entries = match fastdir::list(dir) {
        Ok(v) => v,
        Err(e) => {
            bag.errors.push(Skipped { path: dir.display().to_string(), why: e.to_string() });
            return (node, bag);
        }
    };
    sh.files_seen.fetch_add(entries.len() as u64, Relaxed);
    // this folder's .gitignore applies to it and everything below
    let local_rules;
    let rules = if sh.opts.gitignore {
        match std::fs::read_to_string(dir.join(".gitignore")) {
            Ok(t) => {
                let mut r = rules.clone();
                r.add_lines(&rel, &t, sh.opts.ignore_case);
                local_rules = r;
                &local_rules
            }
            Err(_) => rules,
        }
    } else {
        rules
    };
    let keep = depth < sh.opts.keep_depth;
    let filter = sh.opts.filter.as_ref();
    // 2026-10-16, measured: building the relative path and the full path for EVERY entry made v0.2 slower than v0.1
    // (0.76x on a 473,000-file tree). Now they are built only when an option needs them.
    let track_rel = !rules.is_empty() || sh.opts.gitignore;
    let need_file_path = filter.is_some() || sh.opts.top_files > 0 || sh.opts.disk || sh.opts.hardlinks_windows;
    let mut subdirs: Vec<(PathBuf, String, String, Option<u64>)> = Vec::new();
    for e in entries {
        if !sh.opts.hidden && hidden(&e) {
            bag.hidden += 1;
            continue;
        }
        let erel = if !track_rel { String::new() } else if rel.is_empty() { e.name.clone() } else {
            format!("{rel}/{}", e.name)
        };
        if track_rel && !rules.is_empty() && rules.ignored(&erel, e.kind == EKind::Dir) {
            bag.excluded += 1;
            continue;
        }
        let path = if e.kind != EKind::File || need_file_path { dir.join(&e.name) } else { PathBuf::new() };
        match e.kind {
            EKind::Link(why) => {
                bag.links.push(Skipped { path: path.display().to_string(), why: why.to_string() });
                if let Some(f) = filter {
                    if f.wants(&e.name, Kind::Link, 0, e.mtime) {
                        bag.found.push(Found { path: path.display().to_string(), kind: Kind::Link, bytes: 0,
                                               mtime: e.mtime });
                    }
                }
                if keep {
                    node.children.push(Node { name: e.name, kind: Kind::Link, bytes: 0, disk: 0, files: 0, dirs: 0,
                                              mtime: e.mtime, attrs: e.attrs, children: vec![] });
                }
            }
            EKind::Dir => {
                if sh.opts.one_filesystem && sh.root_dev.is_some() && e.dev.is_some() && e.dev != sh.root_dev {
                    bag.other_fs.push(Skipped { path: path.display().to_string(), why: "another file system".into() });
                    continue;
                }
                subdirs.push((path, erel, e.name, e.mtime));
            }
            EKind::File => {
                let (bytes, disk) = if sh.repeat(&path, &e) {
                    // another name of a file already counted: not a second file. 2026-10-16: the walk counted 2,857
                    // such names as files and disagreed with the master file table (one record per file) by 2,855.
                    sh.hardlink_repeats.fetch_add(1, Relaxed);
                    sh.hardlink_bytes.fetch_add(e.len, Relaxed);
                    continue;
                } else {
                    let disk = match e.alloc {
                        Some(a) => a,
                        None if sh.opts.disk => std::fs::symlink_metadata(&path)
                            .map(|m| os::disk_size(&path, &m, sh.cluster, true)).unwrap_or(e.len),
                        None => os::round_up(e.len, sh.cluster),
                    };
                    (e.len, disk)
                };
                node.bytes += bytes;
                node.disk += disk;
                node.files += 1;
                sh.offer_top(bytes, &path);
                if sh.opts.types {
                    let ext = e.name.rsplit_once('.').filter(|(a, _)| !a.is_empty()).map(|(_, x)| x.to_lowercase())
                        .unwrap_or_default();
                    let t = bag.types.entry(ext).or_default();
                    t.0 += bytes;
                    t.1 += 1;
                }
                if let Some(f) = filter {
                    if f.wants(&e.name, Kind::File, bytes, e.mtime) {
                        bag.found.push(Found { path: path.display().to_string(), kind: Kind::File, bytes,
                                               mtime: e.mtime });
                    }
                }
                if keep {
                    node.children.push(Node { name: e.name, kind: Kind::File, bytes, disk, files: 1, dirs: 0,
                                              mtime: e.mtime, attrs: e.attrs, children: vec![] });
                }
            }
        }
    }
    let kids: Vec<(Node, Bag)> = subdirs.into_par_iter().map(|(p, r, n, m)| {
        let (c, mut b) = walk(&p, r, n, m, depth + 1, rules, sh);
        if let Some(f) = filter {
            if f.wants(&c.name, Kind::Dir, c.bytes, c.mtime) {
                b.found.push(Found { path: p.display().to_string(), kind: Kind::Dir, bytes: c.bytes, mtime: c.mtime });
            }
        }
        (c, b)
    }).collect();
    for (c, b) in kids {
        node.bytes += c.bytes;
        node.disk += c.disk;
        node.files += c.files;
        node.dirs += c.dirs + 1;
        bag.absorb(b);
        if keep {
            node.children.push(c);
        }
    }
    node.children.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    (node, bag)
}
