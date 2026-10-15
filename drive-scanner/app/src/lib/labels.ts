import type { ActionKind, DriveKind, DriveRole, Media, Phase, ScanProfile, Verdict } from '../api';

export const ENGINE_MISSING_TEXT = 'Engine not connected — open this inside the AVNI Drive Scanner app.';

export const ROLE_LABEL: Record<DriveRole, string> = {
  system: 'System',
  external: 'External',
  google_drive: 'Google Drive',
  other: 'Other',
};

export const KIND_LABEL: Record<DriveKind, string> = {
  fixed: 'Fixed',
  removable: 'Removable',
  network: 'Network',
  optical: 'Optical',
  ram: 'RAM disk',
  unknown: 'Unknown',
};

export const MEDIA_LABEL: Record<Media, string> = { hdd: 'HDD', ssd: 'SSD', unknown: 'Unknown' };

export const PROFILE_LABEL: Record<ScanProfile, string> = { quick: 'Quick', deep: 'Deep' };

/** Column / row order for every verdict table. */
export const VERDICT_ORDER: Verdict[] = [
  'keep',
  'protected',
  'transfer',
  'in_drive',
  'duplicate',
  'junk',
  'extract',
  'supabase',
  'review',
];

/** Stays on the drive because Windows or the apps need it. */
export const NEEDED_VERDICTS: Verdict[] = ['keep', 'protected'];

/** Could leave the drive (moved, removed or sent elsewhere). */
export const MOVABLE_VERDICTS: Verdict[] = ['transfer', 'in_drive', 'duplicate', 'junk', 'extract', 'supabase', 'review'];

export const isVerdict = (v: string): v is Verdict => (VERDICT_ORDER as string[]).includes(v);

export const ACTION_ORDER: ActionKind[] = [
  'extract',
  'dedup',
  'transfer',
  'clean_junk',
  'empty_recycle_bin',
  'publish_supabase',
];

export const ACTION_INFO: Record<ActionKind, string> = {
  extract:
    'Unzips archives (zip, 7z, tar, gz, bz2; rar/iso with 7-Zip) next to the archive, opening archives inside archives too. Originals stay.',
  dedup: 'Checks every copy with a full BLAKE3 hash, keeps one, sends the extra copies to the Recycle Bin.',
  transfer: 'Copies files into Google Drive, checks each copy, then sends the original to the Recycle Bin.',
  clean_junk: 'Sends temp files, caches and crash dumps to the Recycle Bin.',
  empty_recycle_bin:
    'Empties the Recycle Bin on the chosen drives — this is what actually frees the space. Cannot be undone.',
  publish_supabase: 'Sends the file list (metadata, hashes, verdicts — not the files) to Supabase.',
};

export const PHASE_STEPS: { phase: Phase; label: string }[] = [
  { phase: 'metadata', label: 'Metadata' },
  { phase: 'rules', label: 'Rules' },
  { phase: 'sampling', label: 'Sampling' },
  { phase: 'verifying', label: 'Verifying' },
  { phase: 'hashing', label: 'Hashing' },
  { phase: 'done', label: 'Done' },
];
