//! The walk: every directory read once, sub-directories in parallel on every core (rayon work stealing), nothing
//! followed that would count a tree twice, every error and every link kept so the report can say what it skipped.
//!
//! Memory stays bounded on any size of tree: below `keep_depth` only the totals travel up, not the entries.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;

use crate::glob::Glob;
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
    /// size on disk (allocated); equal to `bytes` rounded to clusters unless measured
    pub disk: u64,
    pub files: u64,
    pub dirs: u64,
    /// seconds since 1970, of the entry itself
    pub mtime: Option<u64>,
    pub children: Vec<Node>,
}

#[derive(Default)]
pub struct Filter {
    pub names: Vec<Glob>,
    pub exts: Vec<String>,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub newer: Option<u64>,
    pub older: Option<u64>,
    pub kinds: Vec<Kind>,
}

impl Filter {
    fn wants(&self, name: &str, kind: Kind, bytes: u64, mtime: Option<u64>) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&kind) {
            return false;
        }
        if !self.names.is_empty() && !self.names.iter().any(|g| g.matches(name)) {
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

pub struct Shared {
    opts: Opts,
    cluster: u64,
    root_dev: Option<u64>,
    top: Mutex<BinaryHeap<Reverse<(u64, String)>>>,
    top_floor: AtomicU64,
    seen: Mutex<HashSet<(u64, u64)>>,
    pub hardlink_repeats: AtomicU64,
    pub hardlink_bytes: AtomicU64,
    pub links: Mutex<Vec<Skipped>>,
    pub errors: Mutex<Vec<Skipped>>,
    pub other_fs: Mutex<Vec<Skipped>>,
    pub found: Mutex<Vec<Found>>,
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
}

fn secs(t: std::io::Result<SystemTime>) -> Option<u64> {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs())
}

impl Shared {
    fn error(&self, p: &Path, e: impl std::fmt::Display) {
        self.errors.lock().unwrap().push(Skipped { path: p.display().to_string(), why: e.to_string() });
    }

    fn offer_top(&self, bytes: u64, p: &Path) {
        let k = self.opts.top_files;
        if k == 0 || bytes <= self.top_floor.load(Relaxed) && self.top.lock().unwrap().len() >= k {
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

    /// true when this file was already counted under another name
    fn repeat(&self, p: &Path, md: &fs::Metadata) -> bool {
        match os::hardlink_id(p, md, self.opts.hardlinks_windows) {
            Some(id) => !self.seen.lock().unwrap().insert(id),
            None => false,
        }
    }

    fn found(&self, p: &Path, name: &str, kind: Kind, bytes: u64, mtime: Option<u64>) {
        if let Some(f) = &self.opts.filter {
            if f.wants(name, kind, bytes, mtime) {
                self.found.lock().unwrap().push(Found { path: p.display().to_string(), kind, bytes, mtime });
            }
        }
    }
}

/// Walk `root` (a directory, or a single file) and everything under it.
pub fn run(root: &Path, opts: Opts) -> Report {
    let cluster = os::cluster_size(root);
    let md = fs::symlink_metadata(root);
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
        links: Mutex::new(Vec::new()),
        errors: Mutex::new(Vec::new()),
        other_fs: Mutex::new(Vec::new()),
        found: Mutex::new(Vec::new()),
    };
    let name = root.display().to_string();
    let node = match md {
        Err(e) => {
            sh.error(root, e);
            Node { name, kind: Kind::Dir, bytes: 0, disk: 0, files: 0, dirs: 0, mtime: None, children: vec![] }
        }
        Ok(m) if m.is_dir() => walk(root, name, secs(m.modified()), 0, &sh),
        Ok(m) => {
            let disk = os::disk_size(root, &m, sh.cluster, sh.opts.disk);
            Node { name, kind: Kind::File, bytes: m.len(), disk, files: 1, dirs: 0, mtime: secs(m.modified()),
                   children: vec![] }
        }
    };
    let mut top: Vec<(u64, String)> = sh.top.into_inner().unwrap().into_iter().map(|Reverse(x)| x).collect();
    top.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let sorted = |v: Vec<Skipped>| {
        let mut v = v;
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    };
    Report {
        root: node,
        top_files: top,
        links: sorted(sh.links.into_inner().unwrap()),
        errors: sorted(sh.errors.into_inner().unwrap()),
        other_fs: sorted(sh.other_fs.into_inner().unwrap()),
        found: sh.found.into_inner().unwrap(),
        hardlink_repeats: sh.hardlink_repeats.into_inner(),
        hardlink_bytes: sh.hardlink_bytes.into_inner(),
    }
}

fn walk(dir: &Path, name: String, mtime: Option<u64>, depth: usize, sh: &Shared) -> Node {
    let mut node = Node { name, kind: Kind::Dir, bytes: 0, disk: 0, files: 0, dirs: 0, mtime, children: vec![] };
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            sh.error(dir, e);
            return node;
        }
    };
    let keep = depth < sh.opts.keep_depth;
    let mut subdirs: Vec<(PathBuf, String, Option<u64>)> = Vec::new();
    for ent in rd {
        let ent = match ent {
            Ok(e) => e,
            Err(e) => {
                sh.error(dir, e);
                continue;
            }
        };
        let path = ent.path();
        let ename = ent.file_name().to_string_lossy().into_owned();
        let (ft, md) = match (ent.file_type(), ent.metadata()) {
            (Ok(f), Ok(m)) => (f, m),
            (Err(e), _) | (_, Err(e)) => {
                sh.error(&path, e);
                continue;
            }
        };
        let mt = secs(md.modified());
        if let Some(kind) = os::link_kind(&ft, &md) {
            sh.links.lock().unwrap().push(Skipped { path: path.display().to_string(), why: kind.to_string() });
            sh.found(&path, &ename, Kind::Link, 0, mt);
            if keep {
                node.children.push(Node { name: ename, kind: Kind::Link, bytes: 0, disk: 0, files: 0, dirs: 0,
                                          mtime: mt, children: vec![] });
            }
            continue;
        }
        if md.is_dir() {
            if sh.opts.one_filesystem && sh.root_dev.is_some() && os::device(&md) != sh.root_dev {
                sh.other_fs.lock().unwrap().push(Skipped { path: path.display().to_string(),
                                                            why: "another file system".into() });
                continue;
            }
            subdirs.push((path, ename, mt));
            continue;
        }
        let (bytes, disk) = if sh.repeat(&path, &md) {
            sh.hardlink_repeats.fetch_add(1, Relaxed);
            sh.hardlink_bytes.fetch_add(md.len(), Relaxed);
            (0, 0)
        } else {
            (md.len(), os::disk_size(&path, &md, sh.cluster, sh.opts.disk))
        };
        node.bytes += bytes;
        node.disk += disk;
        node.files += 1;
        sh.offer_top(bytes, &path);
        sh.found(&path, &ename, Kind::File, bytes, mt);
        if keep {
            node.children.push(Node { name: ename, kind: Kind::File, bytes, disk, files: 1, dirs: 0, mtime: mt,
                                      children: vec![] });
        }
    }
    let kids: Vec<Node> = subdirs.into_par_iter().map(|(p, n, m)| {
        let c = walk(&p, n, m, depth + 1, sh);
        sh.found(&p, &c.name, Kind::Dir, c.bytes, c.mtime);
        c
    }).collect();
    for mut c in kids {
        node.bytes += c.bytes;
        node.disk += c.disk;
        node.files += c.files;
        node.dirs += c.dirs + 1;
        if keep {
            if depth + 1 >= sh.opts.keep_depth {
                c.children = Vec::new();
            }
            node.children.push(c);
        }
    }
    node.children.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    node
}
