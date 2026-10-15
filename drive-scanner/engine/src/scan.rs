//! One scan, end to end: metadata for every chosen drive (different physical disks in parallel), rules, duplicates,
//! rules again. Progress goes into a shared ScanProgress (polled by the UI) and through `emit` (pushed to the UI).

use crate::hash::{self, Lane, Mode, Stats};
use crate::index::{Group, Index, RawEntry, F_PLACEHOLDER, F_PRIMARY, F_REPARSE};
use crate::model::*;
use crate::rules;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Reporter<'a> {
    pub status: &'a Mutex<ScanProgress>,
    pub emit: &'a (dyn Fn(&ScanProgress) + Sync),
    pub start: Instant,
    last: Mutex<Instant>,
}

impl<'a> Reporter<'a> {
    pub fn new(status: &'a Mutex<ScanProgress>, emit: &'a (dyn Fn(&ScanProgress) + Sync)) -> Self {
        Reporter { status, emit, start: Instant::now(), last: Mutex::new(Instant::now() - Duration::from_secs(1)) }
    }
    /// Change the status; push it to the UI at most 5 times a second unless `force`.
    pub fn update(&self, force: bool, f: impl FnOnce(&mut ScanProgress)) {
        let snap = {
            let mut s = self.status.lock().unwrap();
            f(&mut s);
            s.elapsed_ms = self.start.elapsed().as_millis() as u64;
            let mut last = self.last.lock().unwrap();
            if !force && last.elapsed() < Duration::from_millis(200) {
                return;
            }
            *last = Instant::now();
            s.clone()
        };
        (self.emit)(&snap);
    }
}

pub fn idle_progress() -> ScanProgress {
    ScanProgress {
        phase: Phase::Idle,
        drives: Vec::new(),
        hash_files_done: 0,
        hash_files_total: 0,
        hash_bytes_done: 0,
        hash_bytes_total: 0,
        elapsed_ms: 0,
        eta_ms: None,
        message: String::new(),
    }
}

fn walker_threads(d: &DriveInfo) -> usize {
    match (d.kind, d.media) {
        (DriveKind::Network, _) => 8,
        (_, Media::Ssd) => 16,
        (_, Media::Hdd) => 4,
        _ => 6,
    }
}

/// Metadata of one drive -> (entries, root key, method, notes).
fn read_drive(info: &DriveInfo, slot: usize, rep: &Reporter, cancel: &AtomicBool) -> Result<(Vec<RawEntry>, u64, String, Vec<String>), String> {
    let mut notes = Vec::new();
    if info.mft_capable && info.role != DriveRole::GoogleDrive {
        rep.update(true, |s| s.drives[slot].method = "MFT".into());
        let res = crate::platform::open_volume(&info.id).map_err(|e| format!("cannot open the volume: {e}")).and_then(|vol| {
            let mut cb = |done: u64, _total: u64| rep.update(false, |s| s.drives[slot].files = done);
            crate::ntfs::read_mft(vol, &mut cb, cancel)
        });
        match res {
            Ok(m) => {
                let entries = m
                    .entries
                    .into_iter()
                    .map(|e| RawEntry {
                        key: e.frn,
                        parent: e.parent,
                        name: e.name,
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
                return Ok((entries, crate::ntfs::ROOT_FRN, "MFT".into(), notes));
            }
            Err(e) if e == "cancelled" => return Err(e),
            Err(e) => notes.push(format!("{}: MFT read failed ({e}); used the folder walk instead.", info.id)),
        }
    }
    let method = if info.role == DriveRole::GoogleDrive { "Metadata only (Google Drive)" } else { "Folder walk" };
    rep.update(true, |s| s.drives[slot].method = method.into());
    let cb = |f: u64, d: u64, b: u64| {
        rep.update(false, |s| {
            s.drives[slot].files = f;
            s.drives[slot].dirs = d;
            s.drives[slot].bytes = b;
        })
    };
    let w = crate::walk::walk(&info.root, walker_threads(info), &cb, cancel)?;
    if w.error_count > 0 {
        notes.push(format!(
            "{}: {} folders could not be read (no permission or in use){}",
            info.id,
            w.error_count,
            if crate::platform::is_elevated() { "" } else { " — run as administrator to read more" }
        ));
    }
    Ok((w.entries, w.root_key, method.into(), notes))
}

pub fn run(opts: &ScanOptions, settings: &Settings, all_drives: &[DriveInfo], rep: &Reporter, cancel: &AtomicBool) -> Result<Index, String> {
    let t0 = Instant::now();
    let mut chosen: Vec<DriveInfo> = Vec::new();
    for want in &opts.drives {
        match all_drives.iter().find(|d| d.id.eq_ignore_ascii_case(want)) {
            Some(d) => chosen.push(d.clone()),
            None if std::path::Path::new(want).exists() => chosen.push(crate::drives::drive_for_path(want, "")),
            None => return Err(format!("drive {want} not found")),
        }
    }
    if chosen.is_empty() {
        return Err("no drive chosen".into());
    }
    rep.update(true, |s| {
        *s = idle_progress();
        s.phase = Phase::Metadata;
        s.message = "Reading file lists".into();
        s.drives = chosen.iter().map(|d| DriveProgress { drive: d.id.clone(), ..Default::default() }).collect();
    });

    // Physical disks in parallel; drive letters on the same disk one after another.
    let mut by_disk: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, d) in chosen.iter().enumerate() {
        let key = d.disk.map(|n| format!("disk{n}")).unwrap_or_else(|| d.id.clone());
        match by_disk.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(i),
            None => by_disk.push((key, vec![i])),
        }
    }
    type DriveResult = (usize, Result<(Vec<RawEntry>, u64, String, Vec<String>), String>, u64);
    let results: Mutex<Vec<DriveResult>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for (_, slots) in &by_disk {
            let (chosen, results) = (&chosen, &results);
            s.spawn(move || {
                for &i in slots {
                    let t = Instant::now();
                    let r = read_drive(&chosen[i], i, rep, cancel);
                    let ms = t.elapsed().as_millis() as u64;
                    rep.update(true, |s| {
                        let d = &mut s.drives[i];
                        d.done = true;
                        d.ms = ms;
                        if let Ok((e, _, _, _)) = &r {
                            d.files = e.iter().filter(|x| !x.is_dir).count() as u64;
                            d.dirs = e.iter().filter(|x| x.is_dir).count() as u64;
                            d.bytes = e.iter().filter(|x| !x.is_dir && x.primary).map(|x| x.size).sum();
                        }
                        if let Err(e) = &r {
                            d.error = Some(e.clone());
                        }
                    });
                    results.lock().unwrap().push((i, r, ms));
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|r| r.0);
    let mut index = Index::new(opts.profile);
    for (i, r, ms) in results {
        match r {
            Ok((entries, root_key, method, notes)) => {
                index.notes.extend(notes);
                index.add_drive(chosen[i].clone(), &method, entries, root_key, ms, None);
            }
            Err(e) => {
                index.notes.push(format!("{}: not scanned — {e}", chosen[i].id));
                index.add_drive(chosen[i].clone(), "Failed", Vec::new(), 0, ms, Some(e));
            }
        }
    }
    index.metadata_ms = t0.elapsed().as_millis() as u64;

    rep.update(true, |s| {
        s.phase = Phase::Rules;
        s.message = "Evaluating every file".into();
    });
    rules::evaluate(&mut index, settings);

    let th = Instant::now();
    find_duplicates(&mut index, opts, settings, t0, rep, cancel)?;
    index.hash_ms = th.elapsed().as_millis() as u64;
    rules::apply_duplicates(&mut index);
    rules::apply_in_drive(&mut index);
    if index.drives.iter().any(|d| d.info.role == DriveRole::GoogleDrive) {
        index.notes.push("Google Drive was listed from its folder entries only — no file was downloaded or opened.".into());
    }
    index.total_ms = t0.elapsed().as_millis() as u64;
    rep.update(true, |s| {
        s.phase = Phase::Done;
        s.eta_ms = Some(0);
        s.message = format!("Done in {:.1} s", index.total_ms as f64 / 1000.0);
    });
    Ok(index)
}

fn lanes_for(index: &Index, ids: &[u32]) -> Vec<Lane> {
    let mut lanes: Vec<Lane> = Vec::new();
    for &id in ids {
        let d = index.drive_of(id);
        let key = d.info.disk.map(|n| format!("disk{n}")).unwrap_or_else(|| d.info.id.clone());
        let hdd = d.info.media != Media::Ssd;
        let n = &index.nodes[id as usize];
        let path = index.path_of(id);
        let lcn = if n.lcn == 0 && d.info.media == Media::Hdd { crate::platform::first_lcn(&path).unwrap_or(0) } else { n.lcn };
        let job = hash::Job { id, path, size: n.size, lcn };
        match lanes.iter_mut().find(|l| l.name == key) {
            Some(l) => l.jobs.push(job),
            None => lanes.push(Lane { name: key, hdd, jobs: vec![job] }),
        }
    }
    lanes
}

fn store(index: &mut Index, outs: Vec<hash::Outcome>) -> u64 {
    let mut errors = 0;
    for o in outs {
        if o.error.is_some() {
            errors += 1;
            continue;
        }
        let h = index.hashes.entry(o.id).or_default();
        if o.sample.is_some() {
            h.sample = o.sample;
        }
        if o.full.is_some() {
            h.full = o.full;
        }
        if o.sha256.is_some() {
            h.sha256 = o.sha256;
        }
    }
    errors
}

#[allow(clippy::too_many_arguments)]
fn run_phase(index: &mut Index, ids: &[u32], mode: Mode, phase: Phase, label: &str, deadline: Option<Instant>, rep: &Reporter, cancel: &AtomicBool) -> (u64, usize) {
    let total_bytes: u64 = ids
        .iter()
        .map(|&i| if mode == Mode::Sample { index.nodes[i as usize].size.min(2 * hash::SAMPLE) } else { index.nodes[i as usize].size })
        .sum();
    let stats = Stats::default();
    let started = Instant::now();
    rep.update(true, |s| {
        s.phase = phase;
        s.message = label.to_string();
        s.hash_files_done = 0;
        s.hash_files_total = ids.len() as u64;
        s.hash_bytes_done = 0;
        s.hash_bytes_total = total_bytes;
        s.eta_ms = None;
    });
    let tick = || {
        let fd = stats.files_done.load(Ordering::Relaxed);
        let bd = stats.bytes_done.load(Ordering::Relaxed);
        rep.update(false, |s| {
            s.hash_files_done = fd;
            s.hash_bytes_done = bd;
            let el = started.elapsed().as_millis() as u64;
            s.eta_ms = if fd > 20 && el > 1000 {
                let by_files = el * (ids.len() as u64).saturating_sub(fd) / fd.max(1);
                let by_bytes = (el * total_bytes.saturating_sub(bd)).checked_div(bd).unwrap_or(by_files);
                Some(if mode == Mode::Sample { by_files } else { by_bytes })
            } else {
                None
            };
        })
    };
    let lanes = lanes_for(index, ids);
    let outs = hash::run_lanes(lanes, mode, cancel, deadline, &stats, &tick);
    let done = outs.len();
    let errors = store(index, outs);
    rep.update(true, |s| {
        s.hash_files_done = stats.files_done.load(Ordering::Relaxed);
        s.hash_bytes_done = stats.bytes_done.load(Ordering::Relaxed);
    });
    (errors, done)
}

/// Duplicate candidates, as Avni Clean takes them: every file except system locations and cloud-only placeholders.
/// Protected, program and project files take part on purpose — they are good keepers for a loose copy elsewhere; the
/// tree law refuses them as victims. Google Drive files are never read (that would download them).
fn dedup_candidates(index: &Index, min_size: u64) -> Vec<u32> {
    let mut v = Vec::new();
    for (id, n) in index.nodes.iter().enumerate() {
        if n.is_dir()
            || n.gone()
            || n.size < min_size.max(1)
            || n.flags & F_PRIMARY == 0
            || n.flags & (F_PLACEHOLDER | F_REPARSE) != 0
            || index.drives[n.drive as usize].info.role == DriveRole::GoogleDrive
            || crate::treelaw::excluded_from_candidates(index, id as u32)
        {
            continue;
        }
        v.push(id as u32);
    }
    v
}

fn find_duplicates(index: &mut Index, opts: &ScanOptions, settings: &Settings, t0: Instant, rep: &Reporter, cancel: &AtomicBool) -> Result<(), String> {
    let budget = Duration::from_secs(opts.time_budget_secs.max(60));
    let candidates = dedup_candidates(index, opts.dedup_min_size);
    let mut by_size: HashMap<u64, Vec<u32>> = HashMap::new();
    for id in candidates {
        by_size.entry(index.nodes[id as usize].size).or_default().push(id);
    }
    by_size.retain(|_, v| v.len() > 1);

    if opts.profile == ScanProfile::Deep {
        // Every file on every non-Google-Drive drive, whole content.
        let all: Vec<u32> = index
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                !n.is_dir()
                    && n.size > 0
                    && n.flags & F_PRIMARY != 0
                    && n.flags & (F_PLACEHOLDER | F_REPARSE) == 0
                    && index.drives[n.drive as usize].info.role != DriveRole::GoogleDrive
            })
            .map(|(i, _)| i as u32)
            .collect();
        let (errors, _) = run_phase(index, &all, Mode::Full { sha256: opts.sha256 }, Phase::Hashing, "Full BLAKE3 of every file", None, rep, cancel);
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if errors > 0 {
            index.notes.push(format!("{errors} files could not be read for hashing (in use or no permission)."));
        }
    } else {
        let mut need: Vec<u32> = by_size.values().flatten().copied().collect();
        need.sort_unstable();
        let sample_deadline = t0 + budget.mul_f64(0.6);
        let (errors, done) = run_phase(index, &need, Mode::Sample, Phase::Sampling, "Checking same-size files (first and last 16 KB)", Some(sample_deadline), rep, cancel);
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if done < need.len() {
            let left: u64 = need.iter().filter(|id| !index.hashes.contains_key(id)).map(|id| index.nodes[*id as usize].size).sum();
            index.notes.push(format!(
                "Time budget reached while sampling: {} same-size files ({}) were not checked for duplicates. Raise the time budget to check them.",
                need.len() - done,
                crate::util::human_bytes(left)
            ));
        }
        if errors > 0 {
            index.notes.push(format!("{errors} files could not be read for the duplicate check (in use or no permission)."));
        }
        // Full verification of sampled matches, biggest savings first, in batches until the budget runs out.
        let mut sampled: HashMap<(u64, [u8; 32]), Vec<u32>> = HashMap::new();
        for ids in by_size.values() {
            for &id in ids {
                if let Some(s) = index.hashes.get(&id).and_then(|h| h.sample) {
                    sampled.entry((index.nodes[id as usize].size, s)).or_default().push(id);
                }
            }
        }
        sampled.retain(|_, v| v.len() > 1);
        let mut todo: Vec<(u64, Vec<u32>)> = sampled
            .iter()
            .filter(|((size, _), _)| *size > hash::SMALL)
            .map(|((size, _), v)| (size * (v.len() as u64 - 1), v.iter().copied().filter(|id| index.hashes.get(id).and_then(|h| h.full).is_none()).collect()))
            .collect();
        todo.sort_by_key(|t| std::cmp::Reverse(t.0));
        let deadline = t0 + budget;
        let mut queue: Vec<u32> = todo.into_iter().flat_map(|(_, v)| v).collect();
        let mut verified_all = true;
        while !queue.is_empty() && !cancel.load(Ordering::Relaxed) {
            if Instant::now() >= deadline {
                verified_all = false;
                break;
            }
            let mut batch_bytes = 0u64;
            let mut cut = 0;
            while cut < queue.len() && batch_bytes < (8u64 << 30) {
                batch_bytes += index.nodes[queue[cut] as usize].size;
                cut += 1;
            }
            let batch: Vec<u32> = queue.drain(..cut).collect();
            let (_, done) = run_phase(index, &batch, Mode::Full { sha256: false }, Phase::Verifying, "Confirming duplicates with a full BLAKE3 hash", Some(deadline), rep, cancel);
            if done < batch.len() {
                verified_all = false;
                break;
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if !verified_all {
            index.notes.push(
                "Time budget reached while confirming: some duplicate groups are matched on size + first/last 16 KB only (marked 'sampled'). Remove duplicates checks them fully before anything moves."
                    .into(),
            );
        }
    }

    // Build groups: full hash where every member has one; otherwise the sampled match.
    let mut groups: Vec<(bool, [u8; 32], u64, Vec<u32>)> = Vec::new();
    for ids in by_size.values() {
        let size = index.nodes[ids[0] as usize].size;
        let mut by_sample: HashMap<[u8; 32], Vec<u32>> = HashMap::new();
        for &id in ids {
            let h = index.hashes.get(&id);
            if let Some(k) = h.and_then(|h| if opts.profile == ScanProfile::Deep { h.full } else { h.sample }) {
                by_sample.entry(k).or_default().push(id);
            }
        }
        for (k, members) in by_sample {
            if members.len() < 2 {
                continue;
            }
            if members.iter().all(|m| index.hashes.get(m).and_then(|h| h.full).is_some()) {
                let mut by_full: HashMap<[u8; 32], Vec<u32>> = HashMap::new();
                for m in members {
                    by_full.entry(index.hashes[&m].full.unwrap()).or_default().push(m);
                }
                for (fh, v) in by_full {
                    if v.len() > 1 {
                        groups.push((true, fh, size, v));
                    }
                }
            } else {
                groups.push((false, k, size, members));
            }
        }
    }
    groups.sort_by(|a, b| (b.2 * (b.3.len() as u64 - 1)).cmp(&(a.2 * (a.3.len() as u64 - 1))).then(a.3[0].cmp(&b.3[0])));
    // THE TREE LAW decides every set: which copy is kept, which copies may go, which stay because they are filed.
    let sets: Vec<Vec<u32>> = groups.iter().map(|g| g.3.clone()).collect();
    let decisions: Vec<crate::treelaw::Decision> = {
        let facts = crate::treelaw::TreeFacts::new(index, &sets, &settings.protected_paths, &settings.home, settings.home_clears);
        sets.iter().map(|s| facts.decide(s)).collect()
    };
    index.groups.clear();
    index.group_of.clear();
    index.dup_role.clear();
    let (mut home_used, mut home_agreed, mut home_overruled) = (0u64, 0u64, 0u64);
    let (mut kept_by_law, mut kept_by_law_bytes) = (0u64, 0u64);
    for (i, ((confirmed, hash, size, _), d)) in groups.into_iter().zip(decisions).enumerate() {
        let id = i as u32 + 1;
        home_used += d.home_used as u64;
        home_agreed += d.home_agreed as u64;
        home_overruled += d.home_overruled as u64;
        let mut members = Vec::with_capacity(d.members.len());
        for (m, role, why) in d.members {
            if role == crate::treelaw::Role::Protected {
                kept_by_law += 1;
                kept_by_law_bytes += size;
            }
            let r = index.reason_id(&why);
            index.dup_role.insert(m, (role, r));
            index.group_of.insert(m, id);
            members.push(m);
        }
        index.groups.push(Group { id, size, confirmed, hash, members });
    }
    if kept_by_law > 0 {
        index.notes.push(format!(
            "THE TREE LAW kept {} extra copies ({}) because each is filed inside an organised tree — a filed copy is never the one removed.{}",
            kept_by_law,
            crate::util::human_bytes(kept_by_law_bytes),
            if settings.home_clears { "" } else { " HOME CLEARS (Settings, off) is the only switch that would let copies outside your home places go." }
        ));
    }
    if !settings.home.is_empty() {
        index.notes.push(format!(
            "THE HOME RULE: your home decided {home_used} sets, already agreed with {home_agreed}; THE TREE LAW overruled your home in {home_overruled} sets."
        ));
    }
    index.touch();
    Ok(())
}
