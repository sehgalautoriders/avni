const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] as const;

/** 1024-based, one decimal: 1536 -> "1.5 KB". */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—';
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // 1023.96 KB would print as "1024.0 KB": step up to the next unit instead.
  if (Math.round(value * 10) / 10 >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${UNITS[unit]}`;
}

export const formatCount = (n: number): string => (Number.isFinite(n) ? n.toLocaleString('en-IN') : '—');

export const exactBytes = (bytes: number): string => `${formatCount(bytes)} bytes`;

const pad = (n: number): string => String(n).padStart(2, '0');

/** dd-mm-yyyy HH:mm in local time. 0 (unknown) -> "—". */
export function formatDate(ms: number): string {
  if (!ms) return '—';
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return '—';
  return `${pad(d.getDate())}-${pad(d.getMonth() + 1)}-${d.getFullYear()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 45 s, 3 min 12 s, 2 h 05 min. */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h} h ${pad(m)} min`;
  return `${m} min ${pad(s)} s`;
}

/** Whole-number-ish text for number inputs (no trailing zeros). */
export const trimNumber = (n: number): string => String(Math.round(n * 100) / 100);

export function shortHash(blake3: string | null, sample: string | null): string {
  if (blake3) return blake3.slice(0, 12);
  if (sample) return `sample:${sample.slice(0, 12)}`;
  return '—';
}
