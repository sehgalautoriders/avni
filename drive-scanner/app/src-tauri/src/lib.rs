//! Tauri commands: the bridge between the UI (app/src/api.ts) and the engine (avni-scan). No logic lives here beyond
//! threading, locking and events. Long work (scan, actions, reports) runs on its own thread so the window stays live.

use avni_scan::index::{Index, F_GONE};
use avni_scan::model::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    index: RwLock<Option<Index>>,
    scan_status: Mutex<ScanProgress>,
    action_status: Mutex<ActionProgress>,
    cancel: AtomicBool,
    busy: AtomicBool,
    settings: Mutex<Settings>,
    history: Mutex<Vec<ActionResult>>,
    drives: Mutex<Vec<DriveInfo>>,
    /// Finished files read from the action logs, with the logs' signature they were read at.
    finished: Mutex<Option<FinishedCache>>,
    tables: Mutex<avni_scan::viewer::TableCache>,
}

type Res<T> = Result<T, String>;
/// (log files signature, every finished row read from them).
type FinishedCache = (Vec<(String, u64, i64)>, Vec<FinishedRow>);

/// Runs engine work off the window's main thread (Tauri runs plain commands on it, which would freeze the screen).
async fn blocking<T: Send + 'static>(app: AppHandle, f: impl FnOnce(&AppState) -> Res<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        f(&st)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn history_path() -> std::path::PathBuf {
    avni_scan::platform::app_dir().join("history.json")
}

/// Marks the app busy for one scan or action at a time.
fn claim(st: &AppState) -> Res<()> {
    if st.busy.swap(true, Ordering::SeqCst) {
        return Err("A scan or an action is already running. Wait for it, or press Cancel.".into());
    }
    st.cancel.store(false, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
async fn app_info(app: AppHandle) -> Res<AppInfo> {
    blocking(app, |st| Ok(app_info_now(st))).await
}

fn app_info_now(st: &AppState) -> AppInfo {
    let settings = st.settings.lock().unwrap().clone();
    let drives = st.drives.lock().unwrap().clone();
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        elevated: avni_scan::platform::is_elevated(),
        machine: avni_scan::platform::machine_name(),
        reports_dir: avni_scan::settings::reports_dir(&settings).to_string_lossy().into_owned(),
        settings_path: avni_scan::settings::settings_path().to_string_lossy().into_owned(),
        sevenzip: avni_scan::extract::find_sevenzip(&settings.sevenzip_path).map(|p| p.to_string_lossy().into_owned()),
        gdrive_root: avni_scan::settings::gdrive_root(&settings, &drives),
        os: std::env::consts::OS.to_string(),
    }
}

#[tauri::command]
async fn list_drives(app: AppHandle) -> Res<Vec<DriveInfo>> {
    blocking(app, |st| {
        let d = avni_scan::drives::list_drives();
        *st.drives.lock().unwrap() = d.clone();
        Ok(d)
    })
    .await
}

#[tauri::command]
fn restart_elevated(app: AppHandle) -> Res<()> {
    avni_scan::platform::restart_elevated()?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn start_scan(app: AppHandle, st: State<AppState>, opts: ScanOptions) -> Res<()> {
    if opts.drives.is_empty() {
        return Err("Tick at least one drive.".into());
    }
    claim(&st)?;
    {
        let mut s = st.scan_status.lock().unwrap();
        *s = avni_scan::scan::idle_progress();
        s.phase = Phase::Metadata;
        s.message = "Starting".into();
    }
    std::thread::spawn(move || {
        let st = app.state::<AppState>();
        let settings = st.settings.lock().unwrap().clone();
        let drives = avni_scan::drives::list_drives();
        *st.drives.lock().unwrap() = drives.clone();
        let emit = |p: &ScanProgress| {
            let _ = app.emit("scan-progress", p);
        };
        let rep = avni_scan::scan::Reporter::new(&st.scan_status, &emit);
        let result = avni_scan::scan::run(&opts, &settings, &drives, &rep, &st.cancel);
        let summary = match result {
            Ok(idx) => {
                let s = idx.summary();
                *st.index.write().unwrap() = Some(idx);
                Some(s)
            }
            Err(e) => {
                let cancelled = st.cancel.load(Ordering::SeqCst) || e == "cancelled";
                rep.update(true, |s| {
                    s.phase = if cancelled { Phase::Cancelled } else { Phase::Failed };
                    s.message = if cancelled { "Cancelled — the previous results are kept.".into() } else { e.clone() };
                    s.eta_ms = None;
                });
                None
            }
        };
        st.busy.store(false, Ordering::SeqCst);
        let _ = app.emit("scan-done", summary);
    });
    Ok(())
}

#[tauri::command]
fn cancel(st: State<AppState>) {
    st.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn scan_status(st: State<AppState>) -> ScanProgress {
    st.scan_status.lock().unwrap().clone()
}

#[tauri::command]
async fn get_summary(app: AppHandle) -> Res<Option<Summary>> {
    blocking(app, |st| Ok(st.index.read().unwrap().as_ref().map(|i| i.summary()))).await
}

#[tauri::command]
async fn query_files(app: AppHandle, q: FileQuery) -> Res<FilePage> {
    blocking(app, move |st| {
        Ok(match st.index.read().unwrap().as_ref() {
            Some(i) => i.query(&q),
            None => FilePage { total: 0, bytes: 0, rows: Vec::new() },
        })
    })
    .await
}

#[tauri::command]
async fn query_duplicates(app: AppHandle, offset: u32, limit: u32, confirmed_only: bool) -> Res<DupPage> {
    blocking(app, move |st| {
        Ok(match st.index.read().unwrap().as_ref() {
            Some(i) => i.dup_page(offset, limit.min(500), confirmed_only),
            None => DupPage { total: 0, groups: Vec::new() },
        })
    })
    .await
}

/// Every file the actions finished, where it is now — read from the action logs, newest first.
#[tauri::command]
async fn query_finished(app: AppHandle, q: FinishedQuery) -> Res<FinishedPage> {
    blocking(app, move |st| {
        let settings = st.settings.lock().unwrap().clone();
        let logs = avni_scan::settings::reports_dir(&settings).join("logs");
        let sig = avni_scan::viewer::logs_signature(&logs);
        let mut cache = st.finished.lock().unwrap();
        if cache.as_ref().is_none_or(|(s, _)| *s != sig) {
            *cache = Some((sig, avni_scan::viewer::load_finished(&logs)));
        }
        Ok(avni_scan::viewer::finished_page(&cache.as_ref().unwrap().1, &q))
    })
    .await
}

/// A file or folder, ready to show inside the window. Media files are let through the asset protocol one by one.
#[tauri::command]
async fn preview_file(app: AppHandle, path: String) -> Res<FilePreview> {
    let app2 = app.clone();
    blocking(app, move |st| {
        let cloud: Vec<String> =
            st.drives.lock().unwrap().iter().filter(|d| d.role == DriveRole::GoogleDrive).map(|d| d.root.clone()).collect();
        let p = avni_scan::viewer::preview(&path, &cloud);
        if matches!(p.kind, PreviewKind::Image | PreviewKind::Pdf | PreviewKind::Video | PreviewKind::Audio) {
            app2.asset_protocol_scope().allow_file(&path).map_err(|e| e.to_string())?;
        }
        Ok(p)
    })
    .await
}

/// A page of a CSV report, an action log or an Excel sheet.
#[tauri::command]
async fn read_table(app: AppHandle, q: TableQuery) -> Res<TablePage> {
    blocking(app, move |st| {
        let mut cache = st.tables.lock().unwrap();
        avni_scan::viewer::read_table(&q, &mut cache)
    })
    .await
}

#[tauri::command]
async fn export_reports(app: AppHandle, include_all_files: bool) -> Res<ReportFiles> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let settings = st.settings.lock().unwrap().clone();
        let guard = st.index.read().unwrap();
        let idx = guard.as_ref().ok_or("No scan yet — scan first, then export.")?;
        let summary = idx.summary();
        let dups: Vec<DupGroup> = idx.groups.iter().map(|g| idx.dup_group(g)).filter(|g| g.members.len() > 1).collect();
        let largest = idx.largest(1000);
        let exts = idx.extensions();
        let folders = idx.folders();
        let visit = |f: &mut dyn FnMut(&FileRow) -> anyhow::Result<()>| idx.for_each_row(f);
        let input = avni_scan::report::ReportInput {
            summary: &summary,
            dup_groups: &dups,
            largest: &largest,
            extensions: &exts,
            folders: &folders,
            for_each_row: &visit,
        };
        let opts = avni_scan::report::ReportOptions { include_all_files, stamp: avni_scan::util::stamp_now() };
        let dir = avni_scan::settings::reports_dir(&settings);
        let mut progress = |m: &str| {
            let _ = app.emit("report-progress", m.to_string());
        };
        avni_scan::report::write_reports(&input, &dir, &opts, &mut progress).map_err(|e| format!("Export failed: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn list_reports(app: AppHandle) -> Res<Vec<ReportEntry>> {
    blocking(app, |st| Ok(list_reports_now(st))).await
}

fn list_reports_now(st: &AppState) -> Vec<ReportEntry> {
    let settings = st.settings.lock().unwrap().clone();
    let dir = avni_scan::settings::reports_dir(&settings);
    let mut out = Vec::new();
    for d in [dir.clone(), dir.join("logs")] {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
                if !matches!(ext.as_str(), "xlsx" | "csv") {
                    continue;
                }
                if let Ok(md) = e.metadata() {
                    out.push(ReportEntry {
                        name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        path: p.to_string_lossy().into_owned(),
                        bytes: md.len(),
                        modified_ms: md.modified().map(avni_scan::util::system_time_ms).unwrap_or(0),
                    });
                }
            }
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.modified_ms));
    out
}

#[tauri::command]
fn open_path(path: String) -> Res<()> {
    avni_scan::platform::open_path(&path)
}

#[tauri::command]
fn reveal_path(path: String) -> Res<()> {
    avni_scan::platform::reveal_path(&path)
}

#[tauri::command]
async fn preview_action(app: AppHandle, req: ActionRequest) -> Res<ActionPreview> {
    blocking(app, move |st| {
        let settings = st.settings.lock().unwrap().clone();
        let drives = st.drives.lock().unwrap().clone();
        let guard = st.index.read().unwrap();
        let idx = guard.as_ref().ok_or("No scan yet — scan first.")?;
        Ok(avni_scan::actions::preview(idx, &settings, &drives, &req))
    })
    .await
}

#[tauri::command]
fn run_action(app: AppHandle, st: State<AppState>, req: ActionRequest) -> Res<()> {
    if st.index.read().unwrap().is_none() {
        return Err("No scan yet — scan first.".into());
    }
    claim(&st)?;
    *st.action_status.lock().unwrap() = ActionProgress { kind: Some(req.kind), message: "Starting".into(), ..Default::default() };
    std::thread::spawn(move || {
        let st = app.state::<AppState>();
        let settings = st.settings.lock().unwrap().clone();
        let drives = st.drives.lock().unwrap().clone();
        let last = Mutex::new(Instant::now() - Duration::from_secs(1));
        let progress = |p: &ActionProgress| {
            *st.action_status.lock().unwrap() = p.clone();
            let mut l = last.lock().unwrap();
            if p.finished || l.elapsed() >= Duration::from_millis(200) {
                *l = Instant::now();
                let _ = app.emit("action-progress", p);
            }
        };
        let runner = avni_scan::actions::Runner { settings: &settings, all_drives: &drives, cancel: &st.cancel, progress: &progress };
        let (result, changes) = {
            let guard = st.index.read().unwrap();
            match guard.as_ref() {
                Some(idx) => avni_scan::actions::run(idx, &req, &runner),
                None => return,
            }
        };
        if !changes.gone.is_empty() || !changes.full_hashes.is_empty() {
            if let Some(idx) = st.index.write().unwrap().as_mut() {
                for id in &changes.gone {
                    idx.nodes[*id as usize].flags |= F_GONE;
                }
                for (id, h) in &changes.full_hashes {
                    idx.hashes.entry(*id).or_default().full = Some(*h);
                }
                idx.touch();
            }
        }
        {
            let mut h = st.history.lock().unwrap();
            h.insert(0, result.clone());
            h.truncate(200);
            let _ = std::fs::create_dir_all(avni_scan::platform::app_dir());
            let _ = std::fs::write(history_path(), serde_json::to_string_pretty(&*h).unwrap_or_default());
        }
        {
            let mut s = st.action_status.lock().unwrap();
            s.finished = true;
        }
        st.busy.store(false, Ordering::SeqCst);
        let _ = app.emit("action-done", result);
    });
    Ok(())
}

#[tauri::command]
fn action_status(st: State<AppState>) -> ActionProgress {
    st.action_status.lock().unwrap().clone()
}

#[tauri::command]
fn history(st: State<AppState>) -> Vec<ActionResult> {
    st.history.lock().unwrap().clone()
}

#[tauri::command]
fn get_settings(st: State<AppState>) -> Settings {
    st.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(st: State<AppState>, settings: Settings, supabase_key: Option<String>) -> Res<Settings> {
    let saved = avni_scan::settings::save(&settings, supabase_key.as_deref())?;
    *st.settings.lock().unwrap() = saved.clone();
    Ok(saved)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let past: Vec<ActionResult> = std::fs::read_to_string(history_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let state = AppState {
        index: RwLock::new(None),
        scan_status: Mutex::new(avni_scan::scan::idle_progress()),
        action_status: Mutex::new(ActionProgress { finished: true, ..Default::default() }),
        cancel: AtomicBool::new(false),
        busy: AtomicBool::new(false),
        settings: Mutex::new(avni_scan::settings::load()),
        history: Mutex::new(past),
        drives: Mutex::new(avni_scan::drives::list_drives()),
        finished: Mutex::new(None),
        tables: Mutex::new(Default::default()),
    };
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            app_info,
            list_drives,
            restart_elevated,
            start_scan,
            cancel,
            scan_status,
            get_summary,
            query_files,
            query_duplicates,
            export_reports,
            list_reports,
            open_path,
            reveal_path,
            preview_action,
            run_action,
            action_status,
            history,
            get_settings,
            save_settings,
            query_finished,
            preview_file,
            read_table
        ])
        .run(tauri::generate_context!())
        .expect("AVNI Drive Scanner failed to start");
}
