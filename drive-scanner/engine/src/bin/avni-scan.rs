//! Verification and benchmark tool for the engine (the user-facing program is the AVNI Drive Scanner app).
//!
//!   avni-scan drives
//!   avni-scan mft <C: | image file> [--root <path shown as root>] [--list]
//!   avni-scan walk <folder> [--list]
//!   avni-scan compare <C: | image> <folder that is that volume's root> [--sub <relative folder>] [--max <n>] [--tolerance 0.001]
//!   avni-scan scan <drive or folder>... [--deep] [--sha256] [--budget <secs>] [--min-size <bytes>] [--report <dir>]
//!
//! `compare` reads the same files two independent ways (raw MFT vs the Windows folder listing) and reports every
//! difference in name, size, folder flag and modified time. Exit code 0 = identical.

use avni_scan::index::{Index, RawEntry};
use avni_scan::model::*;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::Instant;

fn arg_val(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1).cloned())
}

fn info_for(target: &str, root: &str, mft: bool) -> DriveInfo {
    let mut d = avni_scan::drives::drive_for_path(root, if mft { "NTFS" } else { "" });
    d.id = target.to_string();
    d.root = root.to_string();
    d.mft_capable = mft;
    d
}

fn mft_index(target: &str, root: &str) -> Result<(Index, u64, avni_scan::ntfs::MftScan), String> {
    let t = Instant::now();
    let vol = avni_scan::platform::open_volume(target).map_err(|e| format!("open {target}: {e}"))?;
    let cancel = AtomicBool::new(false);
    let m = avni_scan::ntfs::read_mft(vol, &mut |_, _| {}, &cancel)?;
    let ms = t.elapsed().as_millis() as u64;
    let entries: Vec<RawEntry> = m
        .entries
        .iter()
        .map(|e| RawEntry {
            key: e.frn,
            parent: e.parent,
            name: e.name.clone(),
            is_dir: e.is_dir,
            size: e.size,
            created: e.created,
            modified: e.modified,
            accessed: e.accessed,
            attrs: e.attrs,
            links: e.links,
            lcn: e.lcn,
            frn: e.frn,
            primary: e.primary,
        })
        .collect();
    let mut idx = Index::new(ScanProfile::Quick);
    idx.add_drive(info_for(target, root, true), "MFT", entries, avni_scan::ntfs::ROOT_FRN, ms, None);
    Ok((idx, ms, m))
}

fn walk_index(root: &str) -> Result<(Index, u64, Vec<String>), String> {
    let t = Instant::now();
    let cancel = AtomicBool::new(false);
    let w = avni_scan::walk::walk(root, 16, &|_, _, _| {}, &cancel)?;
    let ms = t.elapsed().as_millis() as u64;
    let mut idx = Index::new(ScanProfile::Quick);
    idx.add_drive(info_for(root, root, false), "Folder walk", w.entries, w.root_key, ms, None);
    Ok((idx, ms, w.failed_dirs))
}

/// path (lowercased, '/' separators, relative to root) -> (is_dir, size, modified ms)
fn listing(idx: &Index, sub: Option<&str>) -> BTreeMap<String, (bool, u64, i64, String)> {
    let mut m = BTreeMap::new();
    let d = &idx.drives[0];
    let root = d.info.root.trim_end_matches(['\\', '/']).to_string();
    for id in d.root + 1..d.end {
        let n = &idx.nodes[id as usize];
        // NTFS's own metadata files ($MFT, $LogFile, $Extend\...) are invisible to folder listings; leave them out.
        if d.method == "MFT" && n.frn < 16 && n.frn != avni_scan::ntfs::ROOT_FRN {
            continue;
        }
        let p = idx.path_of(id);
        let rel = p.get(root.len()..).unwrap_or(&p).replace('\\', "/");
        let rel = rel.trim_start_matches('/').to_string();
        if d.method == "MFT" && (rel.to_lowercase() == "$extend" || rel.to_lowercase().starts_with("$extend/")) {
            continue;
        }
        if let Some(s) = sub {
            let s = s.replace('\\', "/").to_lowercase();
            let rl = rel.to_lowercase();
            if !rl.starts_with(&format!("{s}/")) {
                continue;
            }
        }
        m.insert(rel.to_lowercase(), (n.is_dir(), if n.is_dir() { 0 } else { n.size }, n.modified, rel));
    }
    m
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let code = match cmd {
        "drives" => {
            println!("{}", serde_json::to_string_pretty(&avni_scan::drives::list_drives()).unwrap());
            0
        }
        "mft" => {
            let target = args.get(1).cloned().unwrap_or_else(|| "C:".into());
            let root = arg_val(&args, "--root").unwrap_or_else(|| format!("{}\\", target.trim_end_matches('\\')));
            match mft_index(&target, &root) {
                Ok((idx, ms, m)) => {
                    let s = idx.summary();
                    println!(
                        "MFT {target}: {} records in use of {} ({} MB MFT), {} files, {} folders, {} in {:.2} s",
                        m.records_in_use,
                        m.records_total,
                        m.mft_bytes >> 20,
                        s.files,
                        s.dirs,
                        avni_scan::util::human_bytes(s.bytes),
                        ms as f64 / 1000.0
                    );
                    if args.iter().any(|a| a == "--list") {
                        for (_, (d, sz, mt, rel)) in listing(&idx, None) {
                            println!("{}\t{}\t{}\t{}", rel, if d { "D" } else { "F" }, sz, mt);
                        }
                    }
                    0
                }
                Err(e) => {
                    eprintln!("MFT failed: {e}");
                    2
                }
            }
        }
        "walk" => {
            let root = args.get(1).cloned().unwrap_or_else(|| ".".into());
            match walk_index(&root) {
                Ok((idx, ms, failed)) => {
                    let s = idx.summary();
                    if !failed.is_empty() {
                        println!("  {} folders could not be listed", failed.len());
                    }
                    println!("Walk {root}: {} files, {} folders, {} in {:.2} s", s.files, s.dirs, avni_scan::util::human_bytes(s.bytes), ms as f64 / 1000.0);
                    0
                }
                Err(e) => {
                    eprintln!("walk failed: {e}");
                    2
                }
            }
        }
        "compare" => {
            let (Some(target), Some(root)) = (args.get(1).cloned(), args.get(2).cloned()) else {
                eprintln!("usage: compare <volume|image> <root folder> [--sub rel] [--max n]");
                std::process::exit(64);
            };
            let sub = arg_val(&args, "--sub");
            let max: usize = arg_val(&args, "--max").and_then(|v| v.parse().ok()).unwrap_or(25);
            let (mi, mms, _) = match mft_index(&target, &root) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("MFT failed: {e}");
                    std::process::exit(2);
                }
            };
            let walk_root = match &sub {
                Some(s) => avni_scan::util::join(&root, s),
                None => root.clone(),
            };
            let (wi, wms, failed) = match walk_index(&walk_root) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("walk failed: {e}");
                    std::process::exit(2);
                }
            };
            let mut a = listing(&mi, sub.as_deref());
            // Folders Windows would not list even for an administrator: drop their contents from the MFT side too.
            let rootn = root.trim_end_matches(['\\', '/']).len();
            let failed_rel: Vec<String> = failed
                .iter()
                .map(|f| f.get(rootn..).unwrap_or(f).replace('\\', "/").trim_start_matches('/').to_lowercase())
                .collect();
            let before = a.len();
            a.retain(|k, _| !failed_rel.iter().any(|f| k.starts_with(&format!("{f}/"))));
            if !failed.is_empty() {
                println!("compare: {} folders not listable by Windows; {} MFT entries under them left out", failed.len(), before - a.len());
            }
            let tolerance: f64 = arg_val(&args, "--tolerance").and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let mut b = listing(&wi, None);
            if let Some(s) = &sub {
                let prefix = s.replace('\\', "/").to_lowercase();
                b = b.into_iter().map(|(k, v)| (format!("{prefix}/{k}"), v)).collect();
            }
            let (mut only_mft, mut only_walk, mut differ) = (Vec::new(), Vec::new(), Vec::new());
            for (k, v) in &a {
                match b.get(k) {
                    None => only_mft.push(v.3.clone()),
                    Some(w) => {
                        // Folder times live in two places on NTFS and may lag in the parent's index; compare files only.
                        if v.0 != w.0 || v.1 != w.1 || (!v.0 && (v.2 - w.2).abs() > 2000) {
                            differ.push(format!("{}: mft(dir={},size={},mtime={}) walk(dir={},size={},mtime={})", v.3, v.0, v.1, v.2, w.0, w.1, w.2));
                        }
                    }
                }
            }
            for (k, v) in &b {
                if !a.contains_key(k) {
                    only_walk.push(v.3.clone());
                }
            }
            println!(
                "compare: MFT {} entries in {:.2} s (whole volume) | walk {} entries in {:.2} s | only in MFT {} | only in walk {} | different {}",
                a.len(),
                mms as f64 / 1000.0,
                b.len(),
                wms as f64 / 1000.0,
                only_mft.len(),
                only_walk.len(),
                differ.len()
            );
            for x in only_mft.iter().take(max) {
                println!("  only MFT : {x}");
            }
            for x in only_walk.iter().take(max) {
                println!("  only walk: {x}");
            }
            for x in differ.iter().take(max) {
                println!("  differ   : {x}");
            }
            let bad = only_mft.len() + only_walk.len() + differ.len();
            let rate = bad as f64 / a.len().max(1) as f64;
            println!("compare: mismatch rate {:.4}% (tolerance {:.4}%)", rate * 100.0, tolerance * 100.0);
            if bad == 0 || rate <= tolerance {
                0
            } else {
                1
            }
        }
        "scan" => {
            let mut targets: Vec<String> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                if args[i].starts_with("--") {
                    if !matches!(args[i].as_str(), "--deep" | "--sha256") {
                        i += 1;
                    }
                } else {
                    targets.push(args[i].clone());
                }
                i += 1;
            }
            let mut settings = avni_scan::settings::load();
            if let Some(p) = arg_val(&args, "--protect") {
                settings.protected_paths.push(p);
            }
            let opts = ScanOptions {
                drives: targets,
                profile: if args.iter().any(|a| a == "--deep") { ScanProfile::Deep } else { ScanProfile::Quick },
                sha256: args.iter().any(|a| a == "--sha256"),
                dedup_min_size: arg_val(&args, "--min-size").and_then(|v| v.parse().ok()).unwrap_or(settings.dedup_min_size),
                time_budget_secs: arg_val(&args, "--budget").and_then(|v| v.parse().ok()).unwrap_or(settings.time_budget_secs),
            };
            let status = Mutex::new(avni_scan::scan::idle_progress());
            let emit = |_: &ScanProgress| {};
            let rep = avni_scan::scan::Reporter::new(&status, &emit);
            let cancel = AtomicBool::new(false);
            let drives = avni_scan::drives::list_drives();
            match avni_scan::scan::run(&opts, &settings, &drives, &rep, &cancel) {
                Ok(idx) => {
                    let s = idx.summary();
                    println!(
                        "SCAN {:?}: {} files, {} folders, {} | metadata {:.2} s, hashing {:.2} s, total {:.2} s | hashed {} files ({}) | duplicate groups {} (confirmed {}), wasted {}",
                        opts.profile,
                        s.files,
                        s.dirs,
                        avni_scan::util::human_bytes(s.bytes),
                        s.metadata_ms as f64 / 1000.0,
                        s.hash_ms as f64 / 1000.0,
                        s.total_ms as f64 / 1000.0,
                        s.hashed_files,
                        avni_scan::util::human_bytes(s.hashed_bytes),
                        s.dup_groups,
                        s.dup_groups_confirmed,
                        avni_scan::util::human_bytes(s.dup_wasted_bytes)
                    );
                    for d in &s.drives {
                        println!("  {} [{}] {} files {} in {:.2} s{}", d.drive.id, d.method, d.files, avni_scan::util::human_bytes(d.bytes), d.scan_ms as f64 / 1000.0, d.error.as_ref().map(|e| format!(" ERROR {e}")).unwrap_or_default());
                        for t in d.totals.iter().filter(|t| t.files > 0) {
                            println!("      {:<26} {:>9} files {:>10}", t.verdict.map(|v| v.label()).unwrap_or(""), t.files, avni_scan::util::human_bytes(t.bytes));
                        }
                    }
                    for n in &s.notes {
                        println!("  note: {n}");
                    }
                    if let Some(dir) = arg_val(&args, "--report") {
                        let t = Instant::now();
                        let largest = idx.largest(1000);
                        let exts = idx.extensions();
                        let folders = idx.folders();
                        let dups: Vec<DupGroup> = idx.groups.iter().map(|g| idx.dup_group(g)).collect();
                        let visit = |f: &mut dyn FnMut(&FileRow) -> anyhow::Result<()>| idx.for_each_row(f);
                        let input = avni_scan::report::ReportInput {
                            summary: &s,
                            dup_groups: &dups,
                            largest: &largest,
                            extensions: &exts,
                            folders: &folders,
                            for_each_row: &visit,
                        };
                        let ro = avni_scan::report::ReportOptions { include_all_files: true, stamp: avni_scan::util::stamp_now() };
                        match avni_scan::report::write_reports(&input, std::path::Path::new(&dir), &ro, &mut |_| {}) {
                            Ok(r) => println!("REPORT {} + {} CSV, {} rows in {:.2} s", r.xlsx, r.csv.len(), r.rows, t.elapsed().as_secs_f64()),
                            Err(e) => println!("REPORT failed: {e}"),
                        }
                    }
                    0
                }
                Err(e) => {
                    eprintln!("scan failed: {e}");
                    2
                }
            }
        }
        _ => {
            eprintln!("commands: drives | mft | walk | compare | scan (see the top of src/bin/avni-scan.rs)");
            64
        }
    };
    std::process::exit(code);
}
