//! Click-driven actions. Nothing here runs on its own: the UI shows a preview first, the user presses Run.
//!
//! Safety rules for every action:
//! - Protected and Keep files are never touched, even if their ids are passed in.
//! - Every file is re-checked against the protected list and against its size/date at scan time; a file that
//!   changed since the scan is skipped ("rescan").
//! - Nothing is deleted for good: removals go to the Recycle Bin (on removable drives, which have none, to
//!   "X:\AVNI Quarantine\<date>"). Only "Empty Recycle Bin" frees space for real, and only when pressed.
//! - Every item is written to a CSV log in the reports folder.

use crate::index::{Index, F_PLACEHOLDER};
use crate::model::*;
use crate::util::{join, local_time_text, now_ms, starts_with_path};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What the caller must apply to the index after an action.
#[derive(Default)]
pub struct Changes {
    pub gone: Vec<u32>,
    pub full_hashes: Vec<(u32, [u8; 32])>,
}

fn allowed(kind: ActionKind, v: Verdict) -> bool {
    match kind {
        ActionKind::Extract => v == Verdict::Extract,
        ActionKind::Dedup => v == Verdict::Duplicate,
        ActionKind::Transfer => matches!(v, Verdict::Transfer | Verdict::InDrive),
        ActionKind::CleanJunk => v == Verdict::Junk,
        ActionKind::EmptyRecycleBin => false,
        ActionKind::PublishSupabase => true,
    }
}

fn in_scope(index: &Index, req: &ActionRequest, id: u32) -> bool {
    req.drives.is_empty() || req.drives.iter().any(|d| d.eq_ignore_ascii_case(&index.drive_of(id).info.id))
}

fn in_recycle_bin(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("$recycle.bin") || p.contains("\\recycler\\")
}

/// The files an action would touch.
pub fn select(index: &Index, req: &ActionRequest) -> Vec<u32> {
    let ok = |id: u32| {
        let n = &index.nodes[id as usize];
        !n.gone()
            && in_scope(index, req, id)
            && (req.kind == ActionKind::PublishSupabase
                || (!n.is_dir()
                    && allowed(req.kind, n.verdict)
                    && n.flags & F_PLACEHOLDER == 0
                    && !(req.kind == ActionKind::CleanJunk && in_recycle_bin(index.dir_path(n.parent)))))
    };
    match &req.ids {
        Some(ids) => ids.iter().copied().filter(|&i| (i as usize) < index.nodes.len() && ok(i)).collect(),
        None => (0..index.nodes.len() as u32).filter(|&i| ok(i)).collect(),
    }
}

fn scoped_drives<'a>(index: &'a Index, req: &ActionRequest) -> Vec<&'a DriveInfo> {
    index
        .drives
        .iter()
        .map(|d| &d.info)
        .filter(|d| d.role != DriveRole::GoogleDrive)
        .filter(|d| req.drives.is_empty() || req.drives.iter().any(|x| x.eq_ignore_ascii_case(&d.id)))
        .collect()
}

pub fn preview(index: &Index, settings: &Settings, _all_drives: &[DriveInfo], req: &ActionRequest) -> ActionPreview {
    let ids = select(index, req);
    let mut p = ActionPreview {
        kind: req.kind,
        files: ids.len() as u64,
        bytes: ids.iter().map(|&i| index.nodes[i as usize].counted_bytes()).sum(),
        sample: ids.iter().take(200).map(|&i| index.row(i)).collect(),
        blocked: Vec::new(),
        warnings: Vec::new(),
        target: None,
    };
    match req.kind {
        ActionKind::Extract => {
            p.target = Some("Next to each archive, in a folder named \"<archive> (extracted)\"".into());
            let seven = crate::extract::find_sevenzip(&settings.sevenzip_path);
            let need7 = ids.iter().filter(|&&i| crate::extract::archive_kind(&index.nodes[i as usize].name) == Some(crate::extract::ArchiveKind::External)).count();
            if need7 > 0 && seven.is_none() {
                p.warnings.push(format!("{need7} archives (rar, iso, xz, ...) need 7-Zip, which is not installed — they will be skipped."));
            }
            p.warnings.push("Extracting adds files to the same drive. Rescan afterwards so the extracted files are checked for duplicates and transferred.".into());
        }
        ActionKind::Dedup => {
            let sampled: BTreeSet<u32> = ids.iter().filter_map(|i| index.group_of.get(i)).filter(|g| !index.groups[(**g - 1) as usize].confirmed).copied().collect();
            if !sampled.is_empty() {
                let bytes: u64 = sampled.iter().map(|g| { let g = &index.groups[(*g - 1) as usize]; g.size * g.members.len() as u64 }).sum();
                p.warnings.push(format!(
                    "{} groups matched on a sample only. Each copy is read in full first ({}) and only exact copies are removed.",
                    sampled.len(),
                    crate::util::human_bytes(bytes)
                ));
            }
            p.target = Some("Recycle Bin (removable drives: \"AVNI Quarantine\" folder on that drive)".into());
            p.warnings.push("Only copies THE TREE LAW marks as loose are removed (dumping ground, pulled out of their folder, beside the kept copy, or tipped into a flat heap). Filed copies, programs, projects and protected folders are never touched.".into());
        }
        ActionKind::Transfer => match crate::settings::transfer_root(settings) {
            Err(e) => p.blocked.push(e),
            Ok(root) => {
                p.target = Some(join(&root, &settings.transfer_folder));
                let c_free = crate::platform::disk_space("C:\\").map(|x| x.0);
                if let Some(free) = c_free {
                    p.warnings.push(format!(
                        "Google Drive for Desktop keeps each file in its cache on C: until it has uploaded. This batch is {}; C: has {} free. The transfer pauses whenever C: falls under {} GB free and continues as uploads finish.",
                        crate::util::human_bytes(p.bytes),
                        crate::util::human_bytes(free),
                        settings.c_reserve_gb
                    ));
                }
                let n_in = ids.iter().filter(|&&i| index.nodes[i as usize].verdict == Verdict::InDrive).count();
                if n_in > 0 {
                    p.warnings.push(format!("{n_in} files already have a same-name, same-size copy on Google Drive: that copy is read and compared first (this downloads it once); if it matches, only the local copy goes to the Recycle Bin."));
                }
                p.warnings.push(format!("Destination account: {} (checked against the AVNI upload account).", settings.gdrive_account.trim()));
                p.warnings.push("Originals go to the Recycle Bin after each copy is checked. Empty the Recycle Bin once Google Drive shows the upload finished.".into());
                if Path::new(MOVED_LEDGER_DIR).is_dir() {
                    p.warnings.push(format!("Every file moved is recorded as MOVED in {MOVED_LEDGER_DIR}\\moved-ledger.csv."));
                } else {
                    p.warnings.push(format!("The MOVED ledger folder {MOVED_LEDGER_DIR} is not on this PC; moves are recorded in this app's own log only."));
                }
            }
        },
        ActionKind::CleanJunk => {
            p.target = Some("Recycle Bin".into());
        }
        ActionKind::EmptyRecycleBin => {
            let mut files = 0;
            let mut bytes = 0;
            for d in scoped_drives(index, req) {
                if let Some((n, b)) = crate::platform::recycle_bin_size(&d.root) {
                    files += n;
                    bytes += b;
                }
                let q = join(&d.root, "AVNI Quarantine");
                if Path::new(&q).is_dir() {
                    p.warnings.push(format!("{q} will be deleted too."));
                }
            }
            p.files = files;
            p.bytes = bytes;
            p.sample.clear();
            p.warnings.push("This cannot be undone.".into());
            p.target = Some(scoped_drives(index, req).iter().map(|d| d.id.clone()).collect::<Vec<_>>().join(", "));
        }
        ActionKind::PublishSupabase => {
            if settings.supabase_url.trim().is_empty() || crate::settings::supabase_key().is_none() {
                p.blocked.push("AUTH REQUIRED: add the Supabase URL and the service role key in Settings.".into());
            }
            p.target = Some(format!("{} → table {}", settings.supabase_url, settings.supabase_table));
            p.bytes = 0;
            p.warnings.push("Only the file list is sent (names, sizes, dates, hashes, verdicts) — never the files.".into());
        }
    }
    if p.files == 0 && p.blocked.is_empty() && req.kind != ActionKind::EmptyRecycleBin {
        p.blocked.push("Nothing to do for this action.".into());
    }
    p
}

struct Log {
    w: csv::Writer<std::fs::File>,
    path: String,
}

impl Log {
    fn open(settings: &Settings, kind: ActionKind) -> Result<Log, String> {
        let dir = crate::settings::reports_dir(settings).join("logs");
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let name = format!("{:?} {}.csv", kind, crate::util::stamp_now_secs());
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        use std::io::Write;
        let _ = f.write_all(b"\xEF\xBB\xBF");
        let mut w = csv::Writer::from_writer(f);
        let _ = w.write_record(["time", "result", "source", "target", "bytes", "message"]);
        Ok(Log { w, path: path.to_string_lossy().into_owned() })
    }
    fn line(&mut self, result: &str, source: &str, target: &str, bytes: u64, msg: &str) {
        let _ = self.w.write_record([local_time_text(now_ms()).as_str(), result, source, target, &bytes.to_string(), msg]);
        let _ = self.w.flush();
    }
}

/// Recycle Bin on fixed drives; "<drive>\AVNI Quarantine\<stamp>\<path>" on removable drives (they have no bin).
fn remove_reversibly(info: &DriveInfo, path: &str, stamp: &str) -> Result<String, String> {
    if info.kind == DriveKind::Removable {
        let rel = path.get(info.root.len()..).unwrap_or(path).trim_start_matches(['\\', '/']);
        let dest = join(&join(&join(&info.root, "AVNI Quarantine"), stamp), rel);
        if let Some(parent) = Path::new(&dest).parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
        Ok(format!("moved to {dest}"))
    } else {
        crate::platform::recycle(&[path.to_string()]).pop().unwrap_or(Err("no result".into())).map(|_| "Recycle Bin".to_string())
    }
}

/// Still the same file as at scan time?
fn unchanged(index: &Index, id: u32, path: &str) -> Result<(), String> {
    let n = &index.nodes[id as usize];
    let md = std::fs::metadata(crate::util::long_path(path)).map_err(|_| "no longer there — rescan".to_string())?;
    if md.len() != n.size {
        return Err("size changed since the scan — rescan".into());
    }
    if let Ok(m) = md.modified() {
        let ms = crate::util::system_time_ms(m);
        // Scan and re-check read the same filesystem field, so any difference (beyond ms rounding) means an edit.
        if n.modified != 0 && (ms - n.modified).abs() > 1 {
            return Err("changed since the scan — rescan".into());
        }
    }
    Ok(())
}

fn protected_now(settings: &Settings, path: &str) -> bool {
    settings.protected_paths.iter().any(|p| starts_with_path(path, p)) || crate::util::ext_of(path) == "ahk"
}

pub struct Runner<'a> {
    pub settings: &'a Settings,
    pub all_drives: &'a [DriveInfo],
    pub cancel: &'a AtomicBool,
    pub progress: &'a (dyn Fn(&ActionProgress) + Sync),
}

pub fn run(index: &Index, req: &ActionRequest, r: &Runner) -> (ActionResult, Changes) {
    let started_ms = now_ms();
    let t0 = Instant::now();
    let mut res = ActionResult { kind: req.kind, started_ms, ms: 0, done: 0, failed: 0, skipped: 0, bytes: 0, log_path: String::new(), messages: Vec::new() };
    let mut ch = Changes::default();
    let pv = preview(index, r.settings, r.all_drives, req);
    if !pv.blocked.is_empty() {
        res.messages = pv.blocked;
        return (res, ch);
    }
    let mut log = match Log::open(r.settings, req.kind) {
        Ok(l) => l,
        Err(e) => {
            res.messages.push(format!("Cannot write the log, so nothing was done: {e}"));
            return (res, ch);
        }
    };
    res.log_path = log.path.clone();
    let ids = select(index, req);
    let mut prog = ActionProgress {
        kind: Some(req.kind),
        total: if req.kind == ActionKind::EmptyRecycleBin { scoped_drives(index, req).len() as u64 } else { ids.len() as u64 },
        bytes_total: pv.bytes,
        ..Default::default()
    };
    (r.progress)(&prog);
    let stamp = crate::util::stamp_now_secs();
    let mut emptied_dirs: BTreeSet<String> = BTreeSet::new();

    match req.kind {
        ActionKind::EmptyRecycleBin => {
            for d in scoped_drives(index, req) {
                prog.current = d.id.clone();
                (r.progress)(&prog);
                match crate::platform::empty_recycle_bin(&d.root) {
                    Ok(()) => {
                        res.done += 1;
                        log.line("ok", &d.root, "", 0, "Recycle Bin emptied");
                    }
                    Err(e) => {
                        res.failed += 1;
                        log.line("failed", &d.root, "", 0, &e);
                        res.messages.push(e);
                    }
                }
                let q = join(&d.root, "AVNI Quarantine");
                if Path::new(&q).is_dir() {
                    match std::fs::remove_dir_all(&q) {
                        Ok(()) => log.line("ok", &q, "", 0, "quarantine deleted"),
                        Err(e) => {
                            res.failed += 1;
                            log.line("failed", &q, "", 0, &e.to_string());
                        }
                    }
                }
                prog.done += 1;
            }
        }
        ActionKind::PublishSupabase => {
            let key = crate::settings::supabase_key().unwrap_or_default();
            let mut cb = |sent: u64, total: u64| {
                prog.done = sent;
                prog.total = total;
                prog.message = format!("{sent} of {total} rows sent");
                (r.progress)(&prog);
            };
            match crate::supabase::publish(index, r.settings, &key, &ids, r.cancel, &mut cb) {
                Ok(n) => {
                    res.done = n;
                    log.line("ok", "", &r.settings.supabase_url, 0, &format!("{n} rows upserted into {}", r.settings.supabase_table));
                }
                Err(e) => {
                    res.failed = 1;
                    log.line("failed", "", &r.settings.supabase_url, 0, &e);
                    res.messages.push(e);
                }
            }
        }
        _ => {
            let gdrive = crate::settings::transfer_root(r.settings).ok();
            let mut buf = vec![0u8; 4 << 20];
            for &id in &ids {
                if r.cancel.load(Ordering::Relaxed) {
                    res.messages.push("Cancelled.".into());
                    break;
                }
                let path = index.path_of(id);
                let size = index.nodes[id as usize].size;
                prog.current = path.clone();
                (r.progress)(&prog);
                let info = &index.drive_of(id).info;
                let outcome: Result<(String, String), String> = if protected_now(r.settings, &path) {
                    Err("protected — skipped".into())
                } else if let Err(e) = unchanged(index, id, &path) {
                    Err(e)
                } else {
                    match req.kind {
                        ActionKind::Extract => extract_one(index, id, &path, r),
                        ActionKind::Dedup => dedup_one(index, id, &path, info, &stamp, &mut buf, r, &mut ch),
                        ActionKind::Transfer => transfer_one(index, id, &path, info, gdrive.as_deref().unwrap_or(""), &stamp, &mut buf, r, &mut prog),
                        ActionKind::CleanJunk => remove_reversibly(info, &path, &stamp).map(|t| (t, "junk removed".into())),
                        _ => Err("not a file action".into()),
                    }
                };
                prog.done += 1;
                match outcome {
                    Ok((target, msg)) => {
                        res.done += 1;
                        res.bytes += size;
                        prog.bytes_done += size;
                        log.line("ok", &path, &target, size, &msg);
                        if req.kind != ActionKind::Extract {
                            ch.gone.push(id);
                            if let Some(p) = Path::new(&path).parent() {
                                emptied_dirs.insert(p.to_string_lossy().into_owned());
                            }
                        }
                    }
                    Err(e) if e.contains("rescan") || e.contains("protected") || e.contains("differs") => {
                        res.skipped += 1;
                        log.line("skipped", &path, "", size, &e);
                    }
                    Err(e) => {
                        res.failed += 1;
                        prog.failed += 1;
                        log.line("failed", &path, "", size, &e);
                        if res.messages.len() < 20 {
                            res.messages.push(format!("{path}: {e}"));
                        }
                    }
                }
                (r.progress)(&prog);
            }
        }
    }
    if req.kind == ActionKind::Transfer {
        // Folders emptied by the move are removed (only if really empty; never a protected folder or a drive root).
        for d in emptied_dirs.iter().rev() {
            let mut cur = PathBuf::from(d);
            loop {
                let s = cur.to_string_lossy().into_owned();
                if protected_now(r.settings, &s) || cur.parent().is_none() || index.drives.iter().any(|x| x.info.root.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(s.trim_end_matches(['\\', '/']))) {
                    break;
                }
                if std::fs::remove_dir(&cur).is_err() {
                    break;
                }
                log.line("ok", &s, "", 0, "empty folder removed");
                match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break,
                }
            }
        }
    }
    res.ms = t0.elapsed().as_millis() as u64;
    prog.finished = true;
    prog.message = format!("{} done, {} skipped, {} failed", res.done, res.skipped, res.failed);
    (r.progress)(&prog);
    (res, ch)
}

fn extract_one(index: &Index, id: u32, path: &str, r: &Runner) -> Result<(String, String), String> {
    let info = &index.drive_of(id).info;
    let reserve = if info.role == DriveRole::System { r.settings.c_reserve_gb << 30 } else { 1 << 30 };
    let free = crate::platform::disk_space(&info.root).map(|x| x.0).unwrap_or(u64::MAX);
    let dest = crate::extract::dest_for(Path::new(path));
    let opts = crate::extract::ExtractOptions {
        max_depth: r.settings.extract_max_depth.max(1),
        max_ratio: r.settings.extract_max_ratio.max(2),
        max_total_bytes: free.saturating_sub(reserve),
        sevenzip: crate::extract::find_sevenzip(&r.settings.sevenzip_path),
        cancel: r.cancel,
    };
    let out = crate::extract::extract_recursive(Path::new(path), &dest, &opts, &mut |_, _| {});
    let msg = format!(
        "{} files, {} extracted{}{}",
        out.files,
        crate::util::human_bytes(out.bytes),
        if out.nested > 0 { format!(", {} nested archives opened", out.nested) } else { String::new() },
        if out.skipped.is_empty() { String::new() } else { format!(", {} entries skipped", out.skipped.len()) }
    );
    match out.error {
        Some(e) if out.files == 0 => Err(e),
        Some(e) => Ok((dest.to_string_lossy().into_owned(), format!("{msg} — stopped: {e}"))),
        None => Ok((dest.to_string_lossy().into_owned(), msg)),
    }
}

fn full_of(index: &Index, id: u32, path: &str, buf: &mut [u8], r: &Runner, ch: &mut Changes) -> Result<[u8; 32], String> {
    if let Some(h) = index.hashes.get(&id).and_then(|h| h.full) {
        return Ok(h);
    }
    if let Some((_, h)) = ch.full_hashes.iter().find(|(i, _)| *i == id) {
        return Ok(*h);
    }
    let (h, _) = crate::hash::full_hash(path, false, buf, r.cancel, &mut |_| {}).map_err(|e| e.to_string())?;
    ch.full_hashes.push((id, h));
    Ok(h)
}

#[allow(clippy::too_many_arguments)]
fn dedup_one(index: &Index, id: u32, path: &str, info: &DriveInfo, stamp: &str, buf: &mut [u8], r: &Runner, ch: &mut Changes) -> Result<(String, String), String> {
    let g = index.group_of.get(&id).ok_or("not in a duplicate group")?;
    let group = &index.groups[(*g - 1) as usize];
    // THE TREE LAW decided this at scan time: members[0] is the keeper, and only a copy it marked loose may go.
    match index.dup_role.get(&id) {
        Some((crate::treelaw::Role::Remove, _)) => {}
        _ => return Err("the tree law keeps this copy - protected".into()),
    }
    let keeper = *group.members.first().ok_or("empty set")?;
    if keeper == id {
        return Err("this is the copy that is kept - protected".into());
    }
    if index.nodes[keeper as usize].gone() {
        return Err("the kept copy was moved by an earlier action - rescan".into());
    }
    let kpath = index.path_of(keeper);
    unchanged(index, keeper, &kpath).map_err(|e| format!("kept copy {kpath}: {e}"))?;
    let hk = full_of(index, keeper, &kpath, buf, r, ch)?;
    let hm = full_of(index, id, path, buf, r, ch)?;
    if hk != hm {
        return Err(format!("content differs from {kpath} (matched on a sample only) - kept"));
    }
    let why = index.dup_role.get(&id).map(|(_, w)| index.reason(*w).to_string()).unwrap_or_default();
    remove_reversibly(info, path, stamp).map(|t| (t, format!("identical to {kpath} — tree law: {why}")))
}

fn copy_hash(src: &str, dest: &str, buf: &mut [u8], cancel: &AtomicBool) -> Result<[u8; 32], String> {
    use std::io::{Read, Write};
    let mut f = std::fs::File::open(crate::util::long_path(src)).map_err(|e| format!("open: {e}"))?;
    let mut o = std::fs::OpenOptions::new().write(true).create_new(true).open(crate::util::long_path(dest)).map_err(|e| format!("create {dest}: {e}"))?;
    let mut h = blake3::Hasher::new();
    let result = (|| {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }
            let n = f.read(buf).map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            o.write_all(&buf[..n]).map_err(|e| format!("write: {e}"))?;
        }
        o.sync_all().map_err(|e| format!("flush: {e}"))?;
        if let Ok(m) = std::fs::metadata(crate::util::long_path(src)).and_then(|m| m.modified()) {
            let _ = o.set_modified(m);
        }
        Ok(())
    })();
    drop(o);
    if let Err(e) = result {
        let _ = std::fs::remove_file(crate::util::long_path(dest));
        return Err(e);
    }
    Ok(*h.finalize().as_bytes())
}

/// C: = OVEN LAW (28-09-2026): every file moved C:->D: or D:->Google Drive is recorded as MOVED here.
pub const MOVED_LEDGER_DIR: &str = "D:\\AVNI DATA\\_MOVED LEDGER";

/// Appends one MOVED row (tag, route, moved_at, original_path, new_path, bytes, modified) when the ledger folder exists.
fn record_moved(info: &DriveInfo, original: &str, new_path: &str, bytes: u64, modified_ms: i64) -> Result<(), String> {
    let dir = Path::new(MOVED_LEDGER_DIR);
    if !dir.is_dir() {
        return Ok(());
    }
    let file = dir.join("moved-ledger.csv");
    let fresh = !file.exists();
    let f = std::fs::OpenOptions::new().create(true).append(true).open(&file).map_err(|e| format!("MOVED ledger: {e}"))?;
    let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(f);
    if fresh {
        let _ = w.write_record(["tag", "route", "moved_at", "original_path", "new_path", "bytes", "modified"]);
    }
    let route = format!("{}->Google Drive", info.id.trim_end_matches(['\\', '/']));
    w.write_record(["MOVED", &route, &local_time_text(now_ms()), original, new_path, &bytes.to_string(), &local_time_text(modified_ms)])
        .and_then(|_| w.flush().map_err(csv::Error::from))
        .map_err(|e| format!("MOVED ledger: {e}"))
}

fn wait_for_c_space(need: u64, r: &Runner, prog: &mut ActionProgress) -> Result<(), String> {
    let reserve = r.settings.c_reserve_gb << 30;
    let mut waited = false;
    loop {
        let Some((free, _)) = crate::platform::disk_space("C:\\") else { return Ok(()) };
        if free >= reserve.saturating_add(need) {
            if waited {
                prog.message.clear();
                (r.progress)(prog);
            }
            return Ok(());
        }
        if r.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        waited = true;
        prog.message = format!(
            "Paused: C: has {} free, under the {} GB reserve. Google Drive is still uploading — continuing by itself when space frees up.",
            crate::util::human_bytes(free),
            r.settings.c_reserve_gb
        );
        (r.progress)(prog);
        std::thread::sleep(Duration::from_secs(10));
    }
}

#[allow(clippy::too_many_arguments)]
fn transfer_one(
    index: &Index,
    id: u32,
    path: &str,
    info: &DriveInfo,
    gdrive: &str,
    stamp: &str,
    buf: &mut [u8],
    r: &Runner,
    prog: &mut ActionProgress,
) -> Result<(String, String), String> {
    let n = &index.nodes[id as usize];
    // Already on Google Drive with the same name and size: compare contents, then only the local copy goes.
    if n.verdict == Verdict::InDrive {
        if let Some(&g) = index.in_drive.get(&id) {
            let gp = index.path_of(g);
            if std::fs::metadata(&gp).map(|m| m.len() == n.size).unwrap_or(false) {
                let mut ch = Changes::default();
                let hl = full_of(index, id, path, buf, r, &mut ch)?;
                let (hg, _) = crate::hash::full_hash(&gp, false, buf, r.cancel, &mut |_| {}).map_err(|e| format!("read Google Drive copy: {e}"))?;
                if hl == hg {
                    let t = remove_reversibly(info, path, stamp)?;
                    let ledger = record_moved(info, path, &gp, n.size, n.modified).err().map(|e| format!("; {e}")).unwrap_or_default();
                    return Ok((gp.clone(), format!("already on Google Drive (contents identical); local copy → {t}{ledger}")));
                }
            }
        }
    }
    wait_for_c_space(n.size, r, prog)?;
    let rel = path.get(info.root.len()..).unwrap_or(path).trim_start_matches(['\\', '/']);
    let letter = info.id.trim_end_matches(['\\', '/']).trim_end_matches(':');
    let letter = letter.rsplit(['\\', '/']).next().unwrap_or(letter);
    let mut dest = join(&join(&join(gdrive, &r.settings.transfer_folder), letter), rel);
    if let Some(parent) = Path::new(&dest).parent() {
        std::fs::create_dir_all(crate::util::long_path(&parent.to_string_lossy())).map_err(|e| format!("create folder on Google Drive: {e}"))?;
    }
    if Path::new(&dest).exists() {
        let same_size = std::fs::metadata(&dest).map(|m| m.len() == n.size).unwrap_or(false);
        if same_size {
            let mut ch = Changes::default();
            let hl = full_of(index, id, path, buf, r, &mut ch)?;
            let (hd, _) = crate::hash::full_hash(&dest, false, buf, r.cancel, &mut |_| {}).map_err(|e| e.to_string())?;
            if hl == hd {
                let t = remove_reversibly(info, path, stamp)?;
                let ledger = record_moved(info, path, &dest, n.size, n.modified).err().map(|e| format!("; {e}")).unwrap_or_default();
                return Ok((dest.clone(), format!("already in the transfer folder (identical); local copy → {t}{ledger}")));
            }
        }
        let p = Path::new(&dest).to_path_buf();
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = p.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
        let parent = p.parent().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
        let mut k = 2;
        loop {
            let cand = join(&parent, &format!("{stem} ({k}){ext}"));
            if !Path::new(&cand).exists() {
                dest = cand;
                break;
            }
            k += 1;
        }
    }
    let hs = copy_hash(path, &dest, buf, r.cancel)?;
    let (hd, _) = crate::hash::full_hash(&dest, false, buf, r.cancel, &mut |_| {}).map_err(|e| format!("re-read the copy: {e}"))?;
    if hs != hd {
        let _ = std::fs::remove_file(&dest);
        return Err("the copy on Google Drive does not match the original — original kept".into());
    }
    let t = remove_reversibly(info, path, stamp)?;
    let ledger = record_moved(info, path, &dest, n.size, n.modified).err().map(|e| format!("; {e}")).unwrap_or_default();
    Ok((dest.clone(), format!("copied and checked (BLAKE3); original → {t}{ledger}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn drive(root: &str, role: DriveRole) -> DriveInfo {
        let mut d = crate::drives::drive_for_path(root, "NTFS");
        d.role = role;
        d
    }

    fn scan(paths: &[&str]) -> Index {
        let mut idx = Index::new(ScanProfile::Quick);
        let cancel = AtomicBool::new(false);
        for p in paths {
            let role = if p.ends_with("gdrive") { DriveRole::GoogleDrive } else { DriveRole::External };
            let w = crate::walk::walk(p, 2, &|_, _, _| {}, &cancel).unwrap();
            idx.add_drive(drive(p, role), "Folder walk", w.entries, w.root_key, 0, None);
        }
        idx
    }

    #[test]
    fn transfer_end_to_end() {
        let t = tempfile::tempdir().unwrap();
        let ext = t.path().join("ext");
        let gd = t.path().join("gdrive");
        let mydrive = gd.join("My Drive");
        std::fs::create_dir_all(ext.join("Photos/2024")).unwrap();
        std::fs::create_dir_all(ext.join("AE Brain")).unwrap();
        std::fs::create_dir_all(mydrive.join("Old")).unwrap();
        std::fs::write(ext.join("Photos/2024/a.jpg"), vec![7u8; 300_000]).unwrap();
        std::fs::write(ext.join("Photos/2024/b.jpg"), vec![8u8; 1000]).unwrap();
        std::fs::write(ext.join("AE Brain/keep.rs"), b"fn main(){}").unwrap();
        std::fs::write(ext.join("changed.txt"), b"before").unwrap();
        // Same name + size already on Google Drive, identical content -> only the local copy goes.
        std::fs::write(ext.join("report.pdf"), vec![1u8; 5000]).unwrap();
        std::fs::write(mydrive.join("Old/report.pdf"), vec![1u8; 5000]).unwrap();
        // Same name + size on Drive but different content -> copied like any other file.
        std::fs::write(ext.join("clash.doc"), vec![2u8; 4000]).unwrap();
        std::fs::write(mydrive.join("clash.doc"), vec![3u8; 4000]).unwrap();

        let (e, g) = (ext.to_string_lossy().into_owned(), gd.to_string_lossy().into_owned());
        let mut idx = scan(&[&e, &g]);
        let mut settings = crate::settings::defaults();
        settings.protected_paths.push(ext.join("AE Brain").to_string_lossy().into_owned());
        settings.gdrive_root = mydrive.to_string_lossy().into_owned();
        // Account law: without the explicit account the transfer is blocked.
        let blocked = preview(&idx, &settings, &[], &ActionRequest { kind: ActionKind::Transfer, ids: None, drives: vec![e.clone()] });
        assert!(blocked.blocked.iter().any(|b| b.contains("upload account")), "{:?}", blocked.blocked);
        settings.gdrive_account = "someone.else@gmail.com".into();
        let wrong = preview(&idx, &settings, &[], &ActionRequest { kind: ActionKind::Transfer, ids: None, drives: vec![e.clone()] });
        assert!(!wrong.blocked.is_empty(), "any other account is refused");
        settings.gdrive_account = "upload-account@example.invalid".into();
        settings.reports_dir = t.path().join("reports").to_string_lossy().into_owned();
        crate::rules::evaluate(&mut idx, &settings);
        crate::rules::apply_in_drive(&mut idx);
        // Changed after the scan -> must be skipped.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(ext.join("changed.txt"), b"after!").unwrap();

        let req = ActionRequest { kind: ActionKind::Transfer, ids: None, drives: vec![e.clone()] };
        let pv = preview(&idx, &settings, &[], &req);
        assert!(pv.blocked.is_empty(), "{:?}", pv.blocked);
        assert_eq!(pv.files, 5, "a.jpg, b.jpg, changed.txt, report.pdf, clash.doc");
        let cancel = AtomicBool::new(false);
        let r = Runner { settings: &settings, all_drives: &[], cancel: &cancel, progress: &|_| {} };
        let (res, ch) = run(&idx, &req, &r);
        assert_eq!((res.done, res.skipped, res.failed), (4, 1, 0), "{:?}", res.messages);
        assert_eq!(ch.gone.len(), 4);

        let letter = e.rsplit(['/', '\\']).next().unwrap();
        let dest = mydrive.join("Laptop Offload").join(letter);
        assert_eq!(std::fs::read(dest.join("Photos/2024/a.jpg")).unwrap(), vec![7u8; 300_000]);
        assert_eq!(std::fs::read(dest.join("Photos/2024/b.jpg")).unwrap(), vec![8u8; 1000]);
        assert_eq!(std::fs::read(dest.join("clash.doc")).unwrap(), vec![2u8; 4000]);
        assert_eq!(std::fs::read(mydrive.join("clash.doc")).unwrap(), vec![3u8; 4000], "Drive's own file untouched");
        assert!(!dest.join("report.pdf").exists(), "already on Drive: not copied again");
        for gone in ["Photos/2024/a.jpg", "Photos/2024/b.jpg", "report.pdf", "clash.doc"] {
            assert!(!ext.join(gone).exists(), "{gone} should have left the external drive");
        }
        assert!(!ext.join("Photos").exists(), "emptied folders are removed");
        assert!(ext.join("AE Brain/keep.rs").exists(), "protected stays");
        assert_eq!(std::fs::read(ext.join("changed.txt")).unwrap(), b"after!", "changed file untouched");
        let log = std::fs::read_to_string(&res.log_path).unwrap();
        assert!(log.contains("changed since the scan"), "{log}");
        assert!(log.contains("already on Google Drive"), "{log}");
    }

    #[test]
    fn protected_and_keep_are_never_selected() {
        let t = tempfile::tempdir().unwrap();
        let ext = t.path().join("ext");
        std::fs::create_dir_all(ext.join("System Volume Information")).unwrap();
        std::fs::write(ext.join("System Volume Information/x"), b"x").unwrap();
        std::fs::write(ext.join("keyboard.ahk"), b"x").unwrap();
        let e = ext.to_string_lossy().into_owned();
        let mut idx = scan(&[&e]);
        let settings = crate::settings::defaults();
        crate::rules::evaluate(&mut idx, &settings);
        let all: Vec<u32> = (0..idx.nodes.len() as u32).collect();
        for kind in [ActionKind::Transfer, ActionKind::Dedup, ActionKind::CleanJunk, ActionKind::Extract] {
            let req = ActionRequest { kind, ids: Some(all.clone()), drives: vec![] };
            assert!(select(&idx, &req).is_empty(), "{kind:?} picked a protected/keep file");
        }
    }
}
