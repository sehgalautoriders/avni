//! Settings file: %LOCALAPPDATA%\Avni\DriveScanner\settings.json. The Supabase key lives next to it in supabase.key,
//! encrypted with Windows DPAPI for the current user; it is never sent to the UI and never written in clear text.

use crate::model::Settings;
use std::path::PathBuf;

pub fn settings_path() -> PathBuf {
    crate::platform::app_dir().join("settings.json")
}

fn key_path() -> PathBuf {
    crate::platform::app_dir().join("supabase.key")
}

pub fn defaults() -> Settings {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\sehga".into());
    let lad = crate::platform::local_app_data();
    Settings {
        protected_paths: vec![
            // Part of the keyboard, not an app. Never touched.
            "C:\\Users\\sehga\\Desktop\\copilotremap.ahk".into(),
            format!("{home}\\Desktop\\copilotremap.ahk"),
            // Live AVNI workspaces: moving them breaks the runtime and its git trees.
            "D:\\AE Brain".into(),
            "C:\\02 - OUR FILES\\Access to all drives\\AVNI".into(),
            lad.join("Avni").to_string_lossy().into_owned(),
            // C: = OVEN LAW (28-09-2026): moved C: folders are junctions into D:\AVNI DATA. Moving or emptying it breaks
            // those C: paths, so it stays protected until Ravi decides how D: is emptied.
            "D:\\AVNI DATA".into(),
        ]
        .into_iter()
        .fold(Vec::new(), |mut v: Vec<String>, p| {
            if !v.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
                v.push(p);
            }
            v
        }),
        gdrive_root: String::new(),
        transfer_folder: "Laptop Offload".into(),
        processed_markers: vec!["processed".into()],
        sevenzip_path: String::new(),
        reports_dir: String::new(),
        supabase_url: String::new(),
        supabase_key_set: false,
        supabase_table: "drive_files".into(),
        dedup_min_size: 256 * 1024,
        time_budget_secs: 15 * 60,
        c_reserve_gb: 20,
        extract_max_depth: 3,
        extract_max_ratio: 50,
        home: Vec::new(),
        home_clears: false,
        gdrive_account: String::new(),
    }
}

impl Default for Settings {
    fn default() -> Self {
        defaults()
    }
}

/// The only Google account AVNI may write to is named in the AVNI Cloud Upload Account Law (26-09-2026). This repo is
/// public, so the address itself is not written here: Ravi types it once in Settings (kept on his PC only) and the app
/// checks it against this SHA-256 fingerprint of the lowercased address.
const UPLOAD_ACCOUNT_SHA256: &str = "d151d5c9b197a95f7f3d4740800cf39c8aa023f1ab3b02e1f422f5da090f0b11";

/// True when `account` is the AVNI upload account.
pub fn is_upload_account(account: &str) -> bool {
    use sha2::Digest;
    let norm = account.trim().to_lowercase();
    if cfg!(test) && norm == "upload-account@example.invalid" {
        return true;
    }
    crate::util::hex(&sha2::Sha256::digest(norm.as_bytes())) == UPLOAD_ACCOUNT_SHA256
}

/// Where Transfer may write. Never auto-detected: the folder and its account must both be set explicitly in Settings.
pub fn transfer_root(s: &Settings) -> Result<String, String> {
    let root = s.gdrive_root.trim();
    if root.is_empty() {
        return Err(
            "Google Drive account law: set the Google Drive folder of the AVNI upload account in Settings (for example G:\\My Drive). The app never picks it on its own."
                .into(),
        );
    }
    if !is_upload_account(&s.gdrive_account) {
        return Err("Google Drive account law: uploads go only to the AVNI upload account. Type that account in Settings, next to the folder.".into());
    }
    if !std::path::Path::new(root).is_dir() {
        return Err(format!("The Google Drive folder {root} is not there — is Google Drive for Desktop running and signed in?"));
    }
    Ok(root.to_string())
}

pub fn load() -> Settings {
    let mut s = std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
        .unwrap_or_else(defaults);
    s.supabase_key_set = key_path().exists();
    s
}

pub fn save(s: &Settings, supabase_key: Option<&str>) -> Result<Settings, String> {
    std::fs::create_dir_all(crate::platform::app_dir()).map_err(|e| e.to_string())?;
    match supabase_key {
        Some("") => {
            let _ = std::fs::remove_file(key_path());
        }
        Some(k) => {
            let enc = crate::platform::protect(k.trim().as_bytes())?;
            std::fs::write(key_path(), enc).map_err(|e| e.to_string())?;
        }
        None => {}
    }
    let mut out = s.clone();
    out.supabase_key_set = key_path().exists();
    out.supabase_url = out.supabase_url.trim().trim_end_matches('/').to_string();
    let text = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
    std::fs::write(settings_path(), text).map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn supabase_key() -> Option<String> {
    let enc = std::fs::read(key_path()).ok()?;
    let raw = crate::platform::unprotect(&enc).ok()?;
    String::from_utf8(raw).ok().filter(|k| !k.is_empty())
}

pub fn reports_dir(s: &Settings) -> PathBuf {
    if s.reports_dir.trim().is_empty() {
        crate::platform::default_reports_dir()
    } else {
        PathBuf::from(s.reports_dir.trim())
    }
}

/// Google Drive for Desktop root: the configured one if it exists, else "<X>:\My Drive" on the Google Drive volume.
pub fn gdrive_root(s: &Settings, drives: &[crate::model::DriveInfo]) -> Option<String> {
    if !s.gdrive_root.trim().is_empty() {
        let p = s.gdrive_root.trim().to_string();
        return std::path::Path::new(&p).is_dir().then_some(p);
    }
    for d in drives.iter().filter(|d| d.role == crate::model::DriveRole::GoogleDrive) {
        for sub in ["My Drive", "Mi unidad", "Meine Ablage", "Mon Drive"] {
            let p = crate::util::join(&d.root, sub);
            if std::path::Path::new(&p).is_dir() {
                return Some(p);
            }
        }
    }
    None
}
