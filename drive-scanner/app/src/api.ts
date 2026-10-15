// The ONE file that talks to the engine. Types mirror engine/src/model.rs 1:1 (snake_case fields, as serde sends them).
// Outside the Tauri window there is no engine: every call throws EngineMissing and the UI says so. No sample data, ever.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type DriveKind = 'fixed' | 'removable' | 'network' | 'optical' | 'ram' | 'unknown';
export type Media = 'hdd' | 'ssd' | 'unknown';
export type DriveRole = 'system' | 'external' | 'google_drive' | 'other';
export type Verdict =
  | 'keep'
  | 'protected'
  | 'transfer'
  | 'in_drive'
  | 'duplicate'
  | 'junk'
  | 'extract'
  | 'supabase'
  | 'review';
export type ScanProfile = 'quick' | 'deep';
export type Phase = 'idle' | 'metadata' | 'rules' | 'sampling' | 'verifying' | 'hashing' | 'done' | 'cancelled' | 'failed';
export type SortKey = 'path' | 'name' | 'size' | 'modified' | 'created' | 'ext' | 'verdict';
export type ActionKind = 'extract' | 'dedup' | 'transfer' | 'clean_junk' | 'empty_recycle_bin' | 'publish_supabase';

export const VERDICT_LABEL: Record<Verdict, string> = {
  keep: 'Keep',
  protected: 'Protected',
  transfer: 'Transfer to Google Drive',
  in_drive: 'Already on Google Drive',
  duplicate: 'Duplicate',
  junk: 'Junk',
  extract: 'Extract archive',
  supabase: 'Processed → Supabase',
  review: 'Review',
};

export const ACTION_LABEL: Record<ActionKind, string> = {
  extract: 'Extract archives (unzip / unblob)',
  dedup: 'Remove duplicates',
  transfer: 'Transfer to Google Drive',
  clean_junk: 'Clean junk',
  empty_recycle_bin: 'Empty Recycle Bin',
  publish_supabase: 'Publish index to Supabase',
};

export interface DriveInfo {
  id: string; // "C:"
  root: string; // "C:\"
  label: string;
  fs: string;
  kind: DriveKind;
  media: Media;
  total_bytes: number;
  free_bytes: number;
  role: DriveRole;
  mft_capable: boolean;
  disk: number | null;
}

export interface ScanOptions {
  drives: string[];
  profile: ScanProfile;
  sha256: boolean;
  dedup_min_size: number;
  time_budget_secs: number;
}

export interface DriveProgress {
  drive: string;
  method: string;
  files: number;
  dirs: number;
  bytes: number;
  done: boolean;
  error: string | null;
  ms: number;
}

export interface ScanProgress {
  phase: Phase;
  drives: DriveProgress[];
  hash_files_done: number;
  hash_files_total: number;
  hash_bytes_done: number;
  hash_bytes_total: number;
  elapsed_ms: number;
  eta_ms: number | null;
  message: string;
}

export interface FileRow {
  id: number;
  drive: string;
  path: string;
  name: string;
  ext: string;
  is_dir: boolean;
  size: number;
  created_ms: number; // unix ms UTC, 0 = unknown
  modified_ms: number;
  accessed_ms: number;
  attributes: number;
  links: number;
  blake3: string | null;
  sha256: string | null;
  sample: string | null;
  dup_group: number | null;
  verdict: Verdict;
  reason: string;
}

export interface VerdictTotal {
  verdict: Verdict | null;
  files: number;
  bytes: number;
}

export interface DriveSummary {
  drive: DriveInfo;
  method: string;
  files: number;
  dirs: number;
  bytes: number;
  scan_ms: number;
  totals: VerdictTotal[];
  error: string | null;
}

export interface Summary {
  scanned_at_ms: number;
  profile: ScanProfile;
  total_ms: number;
  metadata_ms: number;
  hash_ms: number;
  drives: DriveSummary[];
  totals: VerdictTotal[];
  files: number;
  dirs: number;
  bytes: number;
  hashed_files: number;
  hashed_bytes: number;
  dup_groups: number;
  dup_groups_confirmed: number;
  dup_wasted_bytes: number;
  notes: string[];
}

export interface DupGroup {
  id: number;
  size: number;
  confirmed: boolean;
  hash: string;
  members: FileRow[]; // members[0] is kept
  wasted: number;
}

export interface FileQuery {
  drive: string | null;
  verdict: Verdict | null;
  search: string | null;
  ext: string | null;
  min_size: number | null;
  include_dirs: boolean;
  sort: SortKey;
  desc: boolean;
  offset: number;
  limit: number;
}

export interface FilePage {
  total: number;
  bytes: number;
  rows: FileRow[];
}

export interface DupPage {
  total: number;
  groups: DupGroup[];
}

export interface ActionRequest {
  kind: ActionKind;
  ids: number[] | null;
  drives: string[];
}

export interface ActionPreview {
  kind: ActionKind;
  files: number;
  bytes: number;
  sample: FileRow[];
  blocked: string[];
  warnings: string[];
  target: string | null;
}

export interface ActionProgress {
  kind: ActionKind | null;
  done: number;
  total: number;
  bytes_done: number;
  bytes_total: number;
  failed: number;
  current: string;
  message: string;
  finished: boolean;
}

export interface ActionResult {
  kind: ActionKind;
  started_ms: number;
  ms: number;
  done: number;
  failed: number;
  skipped: number;
  bytes: number;
  log_path: string;
  messages: string[];
}

export interface Settings {
  protected_paths: string[];
  gdrive_root: string;
  transfer_folder: string;
  processed_markers: string[];
  sevenzip_path: string;
  reports_dir: string;
  supabase_url: string;
  supabase_key_set: boolean;
  supabase_table: string;
  dedup_min_size: number;
  time_budget_secs: number;
  c_reserve_gb: number;
  extract_max_depth: number;
  extract_max_ratio: number;
  /** THE HOME RULE: where files should live, most wanted first. Only chooses the kept copy; the tree law still rules. */
  home: string[];
  /** HOME CLEARS: lets filed copies outside every home place go. OFF unless Ravi ticks it. */
  home_clears: boolean;
  /** The Google account of gdrive_root. Transfer runs only when this is the AVNI upload account (checked by the engine). */
  gdrive_account: string;
}

export interface ReportFiles {
  folder: string;
  xlsx: string;
  csv: string[];
  rows: number;
  ms: number;
}

export interface ReportEntry {
  name: string;
  path: string;
  bytes: number;
  modified_ms: number;
}

export interface AppInfo {
  version: string;
  elevated: boolean;
  machine: string;
  reports_dir: string;
  settings_path: string;
  sevenzip: string | null;
  gdrive_root: string | null;
  os: string;
}

export class EngineMissing extends Error {
  constructor() {
    super('Engine not connected — open this screen inside the AVNI Drive Scanner app.');
    this.name = 'EngineMissing';
  }
}

export const inTauri = (): boolean => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri()) throw new EngineMissing();
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw e instanceof Error ? e : new Error(String(e));
  }
}

async function on<T>(event: string, cb: (payload: T) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  return listen<T>(event, (e) => cb(e.payload));
}

export const api = {
  appInfo: () => call<AppInfo>('app_info'),
  listDrives: () => call<DriveInfo[]>('list_drives'),
  /** Relaunches the app as administrator (UAC prompt) for the MFT fast path. */
  restartElevated: () => call<void>('restart_elevated'),

  startScan: (opts: ScanOptions) => call<void>('start_scan', { opts }),
  /** Cancels the running scan or action. */
  cancel: () => call<void>('cancel'),
  scanStatus: () => call<ScanProgress>('scan_status'),
  getSummary: () => call<Summary | null>('get_summary'),

  queryFiles: (q: FileQuery) => call<FilePage>('query_files', { q }),
  queryDuplicates: (offset: number, limit: number, confirmedOnly: boolean) =>
    call<DupPage>('query_duplicates', { offset, limit, confirmedOnly }),

  exportReports: (includeAllFiles: boolean) => call<ReportFiles>('export_reports', { includeAllFiles }),
  listReports: () => call<ReportEntry[]>('list_reports'),
  openPath: (path: string) => call<void>('open_path', { path }),
  revealPath: (path: string) => call<void>('reveal_path', { path }),

  previewAction: (req: ActionRequest) => call<ActionPreview>('preview_action', { req }),
  runAction: (req: ActionRequest) => call<void>('run_action', { req }),
  actionStatus: () => call<ActionProgress>('action_status'),
  history: () => call<ActionResult[]>('history'),

  getSettings: () => call<Settings>('get_settings'),
  /** supabaseKey: null = keep the stored key, "" = remove it, anything else = store it (encrypted, never shown again). */
  saveSettings: (settings: Settings, supabaseKey: string | null) =>
    call<Settings>('save_settings', { settings, supabaseKey }),

  onScanProgress: (cb: (p: ScanProgress) => void) => on<ScanProgress>('scan-progress', cb),
  onScanDone: (cb: (s: Summary | null) => void) => on<Summary | null>('scan-done', cb),
  onActionProgress: (cb: (p: ActionProgress) => void) => on<ActionProgress>('action-progress', cb),
  onActionDone: (cb: (r: ActionResult) => void) => on<ActionResult>('action-done', cb),
  onReportProgress: (cb: (msg: string) => void) => on<string>('report-progress', cb),
};
