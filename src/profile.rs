//! The machine and its disks, measured once and kept, so every run uses the thread count that is fastest HERE.
//!
//! Walking a tree is bound by the disk and the file system more than by the processor: an HDD or a USB stick gets
//! slower with many threads; an NVMe drive wants them all. On a hybrid CPU (this laptop: 2 performance + 8
//! efficiency cores) the best count is not "all logical processors" either. So the profile holds:
//!   machine: CPU model, logical processors, P-cores/E-cores, instruction sets, RAM
//!   per volume: its kind (SSD/HDD/removable/network) and the calibrated thread count
//! in %LOCALAPPDATA%\gorilla\machine.json (Linux ~/.local/state/gorilla/machine.json). Local only, never sent.
//! Re-measured by itself when the CPU or the core count changes; `profile --recalibrate` measures again.
//!
//! Honest limit: calibration walks a real folder several times, so after the first pass the file system cache is
//! warm; it measures the walk the way it runs day to day (warm), not a first scan after a reboot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct Machine {
    pub cpu: String,
    pub logical: usize,
    pub perf_cores: usize,
    pub eff_cores: usize,
    pub features: Vec<&'static str>,
    pub ram_gib: f64,
}

pub fn machine() -> Machine {
    let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let (perf_cores, eff_cores) = cores();
    Machine { cpu: cpu_name(), logical, perf_cores, eff_cores, features: features(), ram_gib: ram_gib() }
}

fn features() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut v = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        macro_rules! f {
            ($($n:tt),*) => { $( if std::is_x86_feature_detected!($n) { v.push($n); } )* };
        }
        f!("sse4.2", "popcnt", "avx", "avx2", "bmi2", "avx512f", "sha", "aes");
    }
    v
}

#[cfg(windows)]
fn cpu_name() -> String {
    use std::ffi::c_void;
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(key: isize, sub: *const u16, value: *const u16, flags: u32, typ: *mut u32, data: *mut c_void,
                        len: *mut u32) -> i32;
    }
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (sub, val) = (w(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"), w("ProcessorNameString"));
    let mut buf = vec![0u16; 256];
    let mut len = (buf.len() * 2) as u32;
    const HKLM: isize = 0x8000_0002u32 as i32 as isize;
    let rc = unsafe { RegGetValueW(HKLM, sub.as_ptr(), val.as_ptr(), 0x2, std::ptr::null_mut(),
                                   buf.as_mut_ptr() as *mut c_void, &mut len) };
    if rc != 0 {
        return "unknown".into();
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    String::from_utf16_lossy(&buf[..n]).trim().to_string()
}

#[cfg(not(windows))]
fn cpu_name() -> String {
    std::fs::read_to_string("/proc/cpuinfo").ok()
        .and_then(|t| t.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string()))
        .unwrap_or_else(|| "unknown".into())
}

/// (performance cores, efficiency cores). Not hybrid: (cores, 0).
#[cfg(windows)]
fn cores() -> (usize, usize) {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLogicalProcessorInformationEx(rel: u32, buf: *mut c_void, len: *mut u32) -> i32;
    }
    let mut len = 0u32;
    unsafe { GetLogicalProcessorInformationEx(0, std::ptr::null_mut(), &mut len) };
    let mut buf = vec![0u8; len as usize];
    if unsafe { GetLogicalProcessorInformationEx(0, buf.as_mut_ptr() as *mut c_void, &mut len) } == 0 {
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        return (n, 0);
    }
    // SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX: Relationship u32, Size u32, then PROCESSOR_RELATIONSHIP whose
    // second byte is EfficiencyClass (higher = faster core)
    let mut classes = Vec::new();
    let mut off = 0usize;
    while off + 10 <= buf.len() {
        let size = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()) as usize;
        if size == 0 {
            break;
        }
        classes.push(buf[off + 9]);
        off += size;
    }
    let top = classes.iter().copied().max().unwrap_or(0);
    let p = classes.iter().filter(|&&c| c == top).count();
    if classes.iter().all(|&c| c == top) {
        (p, 0)
    } else {
        (p, classes.len() - p)
    }
}

#[cfg(not(windows))]
fn cores() -> (usize, usize) {
    // Linux exposes per-CPU capacity on hybrid systems; group by core id to count cores, not threads
    let mut caps: BTreeMap<String, u64> = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir("/sys/devices/system/cpu") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if !n.starts_with("cpu") || !n[3..].chars().all(|c| c.is_ascii_digit()) || n.len() == 3 {
                continue;
            }
            let core = std::fs::read_to_string(e.path().join("topology/core_id")).unwrap_or_default();
            let pkg = std::fs::read_to_string(e.path().join("topology/physical_package_id")).unwrap_or_default();
            let cap = std::fs::read_to_string(e.path().join("cpu_capacity")).ok().and_then(|s| s.trim().parse().ok())
                .unwrap_or(1024);
            caps.insert(format!("{}-{}", pkg.trim(), core.trim()), cap);
        }
    }
    if caps.is_empty() {
        return (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1), 0);
    }
    let top = *caps.values().max().unwrap();
    let p = caps.values().filter(|&&c| c == top).count();
    (p, caps.len() - p)
}

#[cfg(windows)]
fn ram_gib() -> f64 {
    #[repr(C)]
    struct MemStatus { len: u32, load: u32, total: u64, avail: u64, pf_t: u64, pf_a: u64, v_t: u64, v_a: u64, x: u64 }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(m: *mut MemStatus) -> i32;
    }
    let mut m = MemStatus { len: 64, load: 0, total: 0, avail: 0, pf_t: 0, pf_a: 0, v_t: 0, v_a: 0, x: 0 };
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        return 0.0;
    }
    (m.total as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0
}

#[cfg(not(windows))]
fn ram_gib() -> f64 {
    std::fs::read_to_string("/proc/meminfo").ok()
        .and_then(|t| t.lines().find(|l| l.starts_with("MemTotal")).and_then(|l| l.split_whitespace().nth(1)
            .and_then(|k| k.parse::<f64>().ok())))
        .map(|kib| (kib / 1048576.0 * 10.0).round() / 10.0).unwrap_or(0.0)
}

/// A stable id and the kind of the volume holding `p`.
pub fn volume(p: &Path) -> (String, String) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetVolumeInformationW(root: *const u16, name: *mut u16, nlen: u32, serial: *mut u32, maxc: *mut u32,
                                     flags: *mut u32, fs: *mut u16, fslen: u32) -> i32;
            fn GetDriveTypeW(root: *const u16) -> u32;
            fn DeviceIoControl(h: *mut c_void, code: u32, inb: *const c_void, inl: u32, outb: *mut c_void, outl: u32,
                               ret: *mut u32, ov: *mut c_void) -> i32;
        }
        let Some(root) = crate::os::win::volume_root(p) else { return ("unknown".into(), "unknown".into()) };
        let mut serial = 0u32;
        unsafe {
            GetVolumeInformationW(root.as_ptr(), std::ptr::null_mut(), 0, &mut serial, std::ptr::null_mut(),
                                  std::ptr::null_mut(), std::ptr::null_mut(), 0);
        }
        let kind = match unsafe { GetDriveTypeW(root.as_ptr()) } {
            2 => "removable".to_string(),
            4 => "network".to_string(),
            3 => {
                // IOCTL_STORAGE_QUERY_PROPERTY, StorageDeviceSeekPenaltyProperty: no seek penalty = SSD
                let r = String::from_utf16_lossy(&root[..root.len() - 1]);
                let dev: Vec<u16> = format!(r"\\.\{}", r.trim_end_matches('\\')).encode_utf16().chain(Some(0)).collect();
                let h = unsafe { crate::os::win::CreateFileW(dev.as_ptr(), 0, 3, std::ptr::null_mut(), 3, 0,
                                                             std::ptr::null_mut()) };
                if h == crate::os::win::INVALID {
                    "fixed".to_string()
                } else {
                    let q: [u32; 3] = [7, 0, 0]; // PropertyId 7 = SeekPenalty, QueryType 0 = standard
                    let mut out = [0u32; 3];
                    let mut ret = 0u32;
                    let ok = unsafe { DeviceIoControl(h, 0x002D_1400, q.as_ptr() as *const c_void, 12,
                                                      out.as_mut_ptr() as *mut c_void, 12, &mut ret, std::ptr::null_mut()) };
                    unsafe { crate::os::win::CloseHandle(h) };
                    if ok != 0 && ret >= 9 {
                        if out[2] & 0xFF != 0 { "hdd".to_string() } else { "ssd".to_string() }
                    } else {
                        "fixed".to_string()
                    }
                }
            }
            _ => "other".to_string(),
        };
        (format!("{serial:08X}"), kind)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(p).map(|m| m.dev()).unwrap_or(0);
        let (maj, min) = ((dev >> 8) & 0xfff, (dev & 0xff) | ((dev >> 12) & 0xfff00));
        let rot = std::fs::read_to_string(format!("/sys/dev/block/{maj}:{min}/queue/rotational"))
            .or_else(|_| std::fs::read_to_string(format!("/sys/dev/block/{maj}:{min}/../queue/rotational")))
            .map(|s| if s.trim() == "1" { "hdd" } else { "ssd" }).unwrap_or("unknown");
        (format!("{maj}:{min}"), rot.to_string())
    }
}

pub fn path() -> PathBuf {
    crate::actions::state_dir().join("machine.json")
}

pub fn load() -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()
}

fn save(v: &Value) {
    let p = path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&p, serde_json::to_string_pretty(v).unwrap_or_default());
}

fn machine_json(m: &Machine) -> Value {
    json!({"cpu": m.cpu, "logical": m.logical, "perf_cores": m.perf_cores, "eff_cores": m.eff_cores,
           "features": m.features, "ram_gib": m.ram_gib})
}

/// The saved profile is still this machine: same CPU name and logical processor count.
fn same_machine(v: &Value, m: &Machine) -> bool {
    v["machine"]["cpu"] == m.cpu.as_str() && v["machine"]["logical"] == m.logical
}

/// Threads to use for a walk under `p`: the calibrated count for its volume, or None (use every core).
pub fn threads_for(p: &Path) -> Option<usize> {
    let v = load()?;
    if !same_machine(&v, &machine()) {
        return None;
    }
    let (id, _) = volume(p);
    v["volumes"][id]["threads"].as_u64().map(|n| n as usize)
}

/// Candidate counts: 1, 2, 4, ... up to the logical processors, plus the P-core count and the core count.
fn candidates(m: &Machine) -> Vec<usize> {
    let mut c = vec![1usize];
    let mut k = 2;
    while k < m.logical {
        c.push(k);
        k *= 2;
    }
    c.push(m.logical);
    if m.eff_cores > 0 {
        c.push(m.perf_cores * 2);
        c.push(m.perf_cores + m.eff_cores);
    }
    c.sort_unstable();
    c.dedup();
    c.retain(|&x| x >= 1 && x <= m.logical);
    c
}

/// Walk `sample` with each candidate count, alternating, `rounds` times; keep the fastest median. Stored per volume.
pub fn calibrate(sample: &Path, rounds: usize, mut progress: impl FnMut(&str)) -> Value {
    let m = machine();
    let (id, kind) = volume(sample);
    let cands = candidates(&m);
    let mut times: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    let walk_once = |n: usize| -> f64 {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(n).build().expect("thread pool");
        let t = Instant::now();
        pool.install(|| {
            let _ = crate::walk::run(sample, crate::walk::Opts::totals_only());
        });
        t.elapsed().as_secs_f64()
    };
    walk_once(m.logical); // warm the cache once, so every candidate sees the same state
    for r in 0..rounds {
        let order: Vec<usize> = if r % 2 == 0 { cands.clone() } else { cands.iter().rev().copied().collect() };
        for &n in &order {
            let s = walk_once(n);
            times.entry(n).or_default().push(s);
            progress(&format!("round {}/{rounds}: {n} threads {s:.3} s", r + 1));
        }
    }
    let med = |v: &Vec<f64>| {
        let mut s = v.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s[s.len() / 2]
    };
    let (best, best_t) = times.iter().map(|(n, v)| (*n, med(v))).min_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).unwrap();
    let all_t = med(&times[&m.logical]);
    let mut v = load().filter(|v| same_machine(v, &m)).unwrap_or_else(|| json!({"volumes": {}}));
    v["machine"] = machine_json(&m);
    v["volumes"][id.clone()] = json!({
        "kind": kind, "threads": best, "median_seconds": best_t, "all_threads_seconds": all_t,
        "sample": sample.display().to_string(), "rounds": rounds,
        "medians": times.iter().map(|(n, t)| (n.to_string(), json!(med(t)))).collect::<serde_json::Map<_, _>>(),
        "measured": crate::output::date(Some(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0))),
    });
    save(&v);
    v
}

pub fn show() -> Value {
    let m = machine();
    let saved = load();
    // sha256_impl: the SHA-256 block function hash/dupes/find --hash will use on this CPU (chosen at run time, sha256.rs)
    json!({"machine": machine_json(&m), "sha256_impl": crate::sha256::implementation(),
           "profile_file": path().display().to_string(),
           "saved": saved.clone().unwrap_or(Value::Null),
           "saved_is_this_machine": saved.as_ref().map(|v| same_machine(v, &m)).unwrap_or(false)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_cover_one_to_all_and_hybrid_counts() {
        let m = Machine { cpu: "x".into(), logical: 12, perf_cores: 2, eff_cores: 8, features: vec![], ram_gib: 0.0 };
        assert_eq!(candidates(&m), vec![1, 2, 4, 8, 10, 12]);
        let old = Machine { cpu: "y".into(), logical: 8, perf_cores: 4, eff_cores: 0, features: vec![], ram_gib: 0.0 };
        assert_eq!(candidates(&old), vec![1, 2, 4, 8]);
    }

    #[test]
    fn this_machine_is_described() {
        let m = machine();
        assert!(m.logical >= 1 && m.perf_cores >= 1);
        assert!(!m.cpu.is_empty());
    }
}
