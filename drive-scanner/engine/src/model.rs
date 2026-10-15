//! Shared data types. Everything the UI sees goes through these; app/src/api.ts mirrors them 1:1.
//! Times are unix milliseconds UTC (0 = unknown). Sizes are bytes. Hashes are lowercase hex.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Ram,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Media {
    Hdd,
    Ssd,
    Unknown,
}

/// What the drive is for, in Ravi's pattern: laptop (C:) is the owner, D:/E: are external and should stay empty,
/// Google Drive is the storage, Supabase holds processed output.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DriveRole {
    System,
    External,
    GoogleDrive,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriveInfo {
    /// "C:" on Windows; the mount path elsewhere (tests).
    pub id: String,
    /// "C:\" on Windows.
    pub root: String,
    pub label: String,
    /// "NTFS", "exFAT", "FAT32", ...
    pub fs: String,
    pub kind: DriveKind,
    pub media: Media,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub role: DriveRole,
    /// NTFS and the raw volume can be opened (needs admin) -> MFT fast path.
    pub mft_capable: bool,
    /// Physical disk number; letters on one disk share it so we never read one spindle from two threads.
    pub disk: Option<u32>,
}

/// The evaluation of one file.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Needed by Windows or installed apps, or it is already in the storage (Google Drive).
    Keep,
    /// On the protected list. Never touched by any action.
    Protected,
    /// User data that belongs on Google Drive.
    Transfer,
    /// Same name and size already on Google Drive; the local copy can go after a check.
    InDrive,
    /// Extra copy; another identical copy is kept.
    Duplicate,
    /// Temp, cache, crash dumps, Recycle Bin contents.
    Junk,
    /// Archive: extract, dedup the contents, then transfer.
    Extract,
    /// Processed output; belongs in Supabase.
    Supabase,
    /// No rule matched; a human decides.
    Review,
}

impl Verdict {
    pub const ALL: [Verdict; 9] = [
        Verdict::Keep,
        Verdict::Protected,
        Verdict::Transfer,
        Verdict::InDrive,
        Verdict::Duplicate,
        Verdict::Junk,
        Verdict::Extract,
        Verdict::Supabase,
        Verdict::Review,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Keep => "Keep",
            Verdict::Protected => "Protected",
            Verdict::Transfer => "Transfer to Google Drive",
            Verdict::InDrive => "Already on Google Drive",
            Verdict::Duplicate => "Duplicate",
            Verdict::Junk => "Junk",
            Verdict::Extract => "Extract archive",
            Verdict::Supabase => "Processed -> Supabase",
            Verdict::Review => "Review",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanProfile {
    /// Metadata of every file + duplicates (size -> sampled hash -> full hash within the time budget).
    Quick,
    /// Quick + full BLAKE3 of every eligible file. Reads every byte: bounded by the drive's read speed.
    Deep,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanOptions {
    pub drives: Vec<String>,
    pub profile: ScanProfile,
    /// Deep only: also SHA-256 in the same read (Google Drive exposes sha256 for matching).
    pub sha256: bool,
    /// Files smaller than this are not checked for duplicates.
    pub dedup_min_size: u64,
    /// Quick: full verification of duplicate candidates stops when the whole scan reaches this many seconds;
    /// unverified groups stay "sampled" and are verified by the Remove-duplicates action before anything moves.
    pub time_budget_secs: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Metadata,
    Rules,
    Sampling,
    Verifying,
    Hashing,
    Done,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DriveProgress {
    pub drive: String,
    /// "MFT", "Folder walk", "Metadata only (Google Drive)".
    pub method: String,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub done: bool,
    pub error: Option<String>,
    pub ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanProgress {
    pub phase: Phase,
    pub drives: Vec<DriveProgress>,
    pub hash_files_done: u64,
    pub hash_files_total: u64,
    pub hash_bytes_done: u64,
    pub hash_bytes_total: u64,
    pub elapsed_ms: u64,
    pub eta_ms: Option<u64>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRow {
    pub id: u32,
    pub drive: String,
    pub path: String,
    pub name: String,
    /// Lowercase, without the dot. "" when none.
    pub ext: String,
    pub is_dir: bool,
    pub size: u64,
    pub created_ms: i64,
    pub modified_ms: i64,
    pub accessed_ms: i64,
    /// Windows FILE_ATTRIBUTE_* bits.
    pub attributes: u32,
    pub links: u16,
    /// Full-content BLAKE3.
    pub blake3: Option<String>,
    pub sha256: Option<String>,
    /// BLAKE3 of size + first/middle/last 16 KB.
    pub sample: Option<String>,
    pub dup_group: Option<u32>,
    pub verdict: Verdict,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct VerdictTotal {
    pub verdict: Option<Verdict>,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriveSummary {
    pub drive: DriveInfo,
    pub method: String,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub scan_ms: u64,
    pub totals: Vec<VerdictTotal>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub scanned_at_ms: i64,
    pub profile: ScanProfile,
    pub total_ms: u64,
    pub metadata_ms: u64,
    pub hash_ms: u64,
    pub drives: Vec<DriveSummary>,
    pub totals: Vec<VerdictTotal>,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub hashed_files: u64,
    pub hashed_bytes: u64,
    pub dup_groups: u64,
    pub dup_groups_confirmed: u64,
    /// Bytes freed if every extra copy went away.
    pub dup_wasted_bytes: u64,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DupGroup {
    pub id: u32,
    pub size: u64,
    /// true = full BLAKE3 matched; false = only the sampled hash matched so far.
    pub confirmed: bool,
    pub hash: String,
    /// members[0] is the copy that is kept.
    pub members: Vec<FileRow>,
    pub wasted: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    Path,
    Name,
    Size,
    Modified,
    Created,
    Ext,
    Verdict,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileQuery {
    pub drive: Option<String>,
    pub verdict: Option<Verdict>,
    /// Case-insensitive substring of the name (or of the full path when it contains a '\' or '/').
    pub search: Option<String>,
    pub ext: Option<String>,
    pub min_size: Option<u64>,
    pub include_dirs: bool,
    pub sort: SortKey,
    pub desc: bool,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilePage {
    pub total: u64,
    pub bytes: u64,
    pub rows: Vec<FileRow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DupPage {
    pub total: u64,
    pub groups: Vec<DupGroup>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtTotal {
    pub ext: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FolderTotal {
    pub drive: String,
    pub path: String,
    pub files: u64,
    pub bytes: u64,
    pub verdict: Verdict,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Unzip / unblob: extract archives (nested too) next to the archive.
    Extract,
    /// Full-verify duplicate groups, keep one copy, rest to Recycle Bin.
    Dedup,
    /// Copy to Google Drive, verify, source to Recycle Bin.
    Transfer,
    /// Junk to Recycle Bin.
    CleanJunk,
    /// Empty the Recycle Bin of the chosen drives (frees the space for real).
    EmptyRecycleBin,
    /// Push the file index (metadata, hashes, verdicts) to Supabase.
    PublishSupabase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionRequest {
    pub kind: ActionKind,
    /// Specific file ids; None = every file whose verdict fits the action on the chosen drives.
    pub ids: Option<Vec<u32>>,
    /// Drives in scope; empty = all scanned drives.
    pub drives: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionPreview {
    pub kind: ActionKind,
    pub files: u64,
    pub bytes: u64,
    /// First 200 items.
    pub sample: Vec<FileRow>,
    /// Why the action cannot run (missing Google Drive, missing key, ...). Non-empty = Run is disabled.
    pub blocked: Vec<String>,
    pub warnings: Vec<String>,
    /// Where things go, when that applies.
    pub target: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ActionProgress {
    pub kind: Option<ActionKind>,
    pub done: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub failed: u64,
    pub current: String,
    pub message: String,
    pub finished: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionResult {
    pub kind: ActionKind,
    pub started_ms: i64,
    pub ms: u64,
    pub done: u64,
    pub failed: u64,
    pub skipped: u64,
    pub bytes: u64,
    pub log_path: String,
    pub messages: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Never touched. Prefix match, case-insensitive. A file path matches too.
    pub protected_paths: Vec<String>,
    /// Google Drive for Desktop root, e.g. "G:\My Drive". "" = auto-detect.
    pub gdrive_root: String,
    /// Folder inside Google Drive that receives transfers, e.g. "Laptop Offload".
    pub transfer_folder: String,
    /// Folder names that mean "processed output" (-> Supabase), e.g. "processed".
    pub processed_markers: Vec<String>,
    /// "" = auto-detect C:\Program Files\7-Zip\7z.exe.
    pub sevenzip_path: String,
    /// "" = %LOCALAPPDATA%\Avni\reports\DriveScanner.
    pub reports_dir: String,
    pub supabase_url: String,
    /// The key itself never leaves the engine; the UI only sees whether one is stored.
    pub supabase_key_set: bool,
    pub supabase_table: String,
    pub dedup_min_size: u64,
    pub time_budget_secs: u64,
    /// Keep at least this much free on C: while transferring (Google Drive for Desktop caches uploads on C:).
    pub c_reserve_gb: u64,
    /// Unblob: how deep nested archives are opened.
    pub extract_max_depth: u32,
    /// Unblob: stop when extracted bytes exceed archive bytes x this (zip-bomb guard).
    pub extract_max_ratio: u64,
    /// THE HOME RULE (Avni Clean, 29-08-2026): where files should live, most wanted first ("D:\" or a folder).
    /// It only chooses which copy of a duplicate set is KEPT; the tree law still decides what may go.
    pub home: Vec<String>,
    /// HOME CLEARS: lets a FILED copy outside every home place go, when the kept copy is filed inside a home place.
    /// OFF unless Ravi ticks it himself.
    pub home_clears: bool,
    /// Google Drive account the transfer folder belongs to. AVNI Cloud Upload Account Law: must be the AVNI upload
    /// account, chosen explicitly — never inferred from whichever account is mounted.
    pub gdrive_account: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportFiles {
    pub folder: String,
    pub xlsx: String,
    pub csv: Vec<String>,
    pub rows: u64,
    pub ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportEntry {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub modified_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppInfo {
    pub version: String,
    pub elevated: bool,
    pub machine: String,
    pub reports_dir: String,
    pub settings_path: String,
    pub sevenzip: Option<String>,
    pub gdrive_root: Option<String>,
    pub os: String,
}

// ---- Finished files, previews and in-app tables (app/src/api.ts mirrors these) ----

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishedResult {
    Ok,
    Skipped,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinishedRow {
    pub id: u32,
    pub when_ms: i64,
    pub action: ActionKind,
    pub result: FinishedResult,
    pub name: String,
    pub source: String,
    pub target: String,
    pub now_at: String,
    pub exists: bool,
    pub kept_at: String,
    pub bytes: u64,
    pub message: String,
    pub log_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinishedQuery {
    pub action: Option<ActionKind>,
    pub result: Option<FinishedResult>,
    pub search: Option<String>,
    pub log_path: Option<String>,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinishedPage {
    pub total: u64,
    pub bytes: u64,
    pub rows: Vec<FinishedRow>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    Image,
    Pdf,
    Video,
    Audio,
    Text,
    Folder,
    Table,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviewEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilePreview {
    pub path: String,
    pub name: String,
    pub kind: PreviewKind,
    pub exists: bool,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: i64,
    pub text: Option<String>,
    pub truncated: bool,
    pub entries: Option<Vec<PreviewEntry>>,
    pub entries_total: u64,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableQuery {
    pub path: String,
    pub sheet: Option<String>,
    pub search: Option<String>,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TablePage {
    pub path: String,
    pub sheets: Vec<String>,
    pub sheet: Option<String>,
    pub headers: Vec<String>,
    pub total: u64,
    pub rows: Vec<Vec<String>>,
    pub note: Option<String>,
}
