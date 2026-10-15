import { useId, useState } from 'react';
import { useMutation } from '@tanstack/react-query';
import { api, type DriveInfo, type ScanOptions, type ScanProfile } from '../api';
import { Button, CheckField, ErrorText, INPUT_CLASS, Loading, Panel, Size, TableFrame, TickBox } from '../components/ui';
import { trimNumber } from '../lib/format';
import { KIND_LABEL, MEDIA_LABEL, ROLE_LABEL } from '../lib/labels';
import { useDrives, useSettings } from '../lib/queries';
import { isScanActive, useApp } from '../store';

function DriveRow({ drive, checked, onToggle }: { drive: DriveInfo; checked: boolean; onToggle: () => void }) {
  const used = Math.max(0, drive.total_bytes - drive.free_bytes);
  return (
    <tr>
      <td>
        <TickBox checked={checked} onChange={onToggle} aria-label={`Scan drive ${drive.id}`} />
      </td>
      <td className="font-semibold">{drive.id}</td>
      <td>
        {drive.label || <span className="text-muted">—</span>}
        {drive.role === 'google_drive' && (
          <div className="text-xs text-muted">metadata only — files are never downloaded</div>
        )}
      </td>
      <td>{ROLE_LABEL[drive.role]}</td>
      <td>{drive.fs || '—'}</td>
      <td>{KIND_LABEL[drive.kind]}</td>
      <td title={drive.disk !== null ? `Physical disk ${drive.disk}` : undefined}>{MEDIA_LABEL[drive.media]}</td>
      <td className="num">
        <Size bytes={drive.total_bytes} />
      </td>
      <td className="num">
        <Size bytes={used} />
      </td>
      <td className="num">
        <Size bytes={drive.free_bytes} />
      </td>
      <td>{drive.mft_capable ? 'Yes' : 'No'}</td>
    </tr>
  );
}

/** "Skip duplicate check for files smaller than [256] KB": a sentence with a number box inside it. */
function InlineNumber({
  before,
  after,
  value,
  onChange,
}: {
  before: string;
  after: string;
  value: string;
  onChange: (v: string) => void;
}) {
  const id = useId();
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm">
      <label htmlFor={id}>{before}</label>
      <input
        id={id}
        type="number"
        min={0}
        step="any"
        inputMode="decimal"
        className={`${INPUT_CLASS} w-28`}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
      <span aria-hidden="true">{after}</span>
    </div>
  );
}

function parseNonNegative(text: string): number | null {
  if (text.trim() === '') return null;
  const n = Number(text);
  return Number.isFinite(n) && n >= 0 ? n : null;
}

export function DrivesTab() {
  const drivesQ = useDrives();
  const settingsQ = useSettings();
  const scanning = useApp((s) => isScanActive(s.scan));
  const picked = useApp((s) => s.selectedDrives);
  const setPicked = useApp((s) => s.setSelectedDrives);
  const setTab = useApp((s) => s.setTab);

  const [profile, setProfile] = useState<ScanProfile>('quick');
  const [sha256, setSha256] = useState(false);
  // null = "use the saved default"; the text is only kept once the user types.
  const [minKb, setMinKb] = useState<string | null>(null);
  const [budgetMin, setBudgetMin] = useState<string | null>(null);

  const drives = drivesQ.data ?? [];
  const ids = new Set(drives.map((d) => d.id));
  const selected = picked.filter((id) => ids.has(id));
  const allChecked = drives.length > 0 && selected.length === drives.length;

  const defaults = settingsQ.data;
  const kbText = minKb ?? (defaults ? trimNumber(defaults.dedup_min_size / 1024) : '');
  const minText = budgetMin ?? (defaults ? trimNumber(defaults.time_budget_secs / 60) : '');
  const kb = parseNonNegative(kbText);
  const minutes = parseNonNegative(minText);

  const start = useMutation({
    mutationFn: async (opts: ScanOptions) => {
      await api.startScan(opts);
      try {
        useApp.getState().setScan(await api.scanStatus());
      } catch {
        // the live feed will catch up on its own
      }
    },
    onSuccess: () => setTab('progress'),
  });

  const toggle = (id: string): void =>
    setPicked(selected.includes(id) ? selected.filter((x) => x !== id) : [...selected, id]);

  const canStart = selected.length > 0 && kb !== null && minutes !== null && !scanning;
  const onStart = (): void => {
    if (kb === null || minutes === null) return;
    start.mutate({
      drives: selected,
      profile,
      sha256: profile === 'deep' && sha256,
      dedup_min_size: Math.round(kb * 1024),
      time_budget_secs: Math.round(minutes * 60),
    });
  };

  return (
    <Panel label="Drives">
      <div className="flex flex-wrap items-center gap-3">
        <p className="text-sm text-muted">Tick the drives you want to scan, then press Start scan.</p>
        <Button className="ml-auto" busy={drivesQ.isFetching} busyLabel="Refreshing…" onClick={() => void drivesQ.refetch()}>
          Refresh drives
        </Button>
      </div>
      <ErrorText error={drivesQ.error} />

      <TableFrame label="Drives on this computer">
        {drivesQ.isPending ? (
          <div className="p-4">
            <Loading text="Looking for drives…" />
          </div>
        ) : drives.length === 0 ? (
          <p className="p-4 text-sm">No drives were found.</p>
        ) : (
          <table className="tbl">
            <thead>
              <tr>
                <th>
                  <TickBox
                    checked={allChecked}
                    indeterminate={selected.length > 0 && !allChecked}
                    onChange={() => setPicked(allChecked ? [] : drives.map((d) => d.id))}
                    aria-label="Select all drives"
                  />
                </th>
                <th>Drive</th>
                <th>Label</th>
                <th>Role</th>
                <th>File system</th>
                <th>Type</th>
                <th>Disk</th>
                <th className="num">Size</th>
                <th className="num">Used</th>
                <th className="num">Free</th>
                <th title="Reads the NTFS master file table: seconds instead of minutes">Fast mode (MFT)</th>
              </tr>
            </thead>
            <tbody>
              {drives.map((d) => (
                <DriveRow key={d.id} drive={d} checked={selected.includes(d.id)} onToggle={() => toggle(d.id)} />
              ))}
            </tbody>
          </table>
        )}
      </TableFrame>

      <form
        className="flex shrink-0 flex-col gap-3 rounded-md border border-line bg-surface p-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (canStart) onStart();
        }}
      >
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1 text-sm font-bold">Scan type</legend>
          <label className="flex items-start gap-2 text-sm">
            <input
              type="radio"
              name="profile"
              className="mt-0.5 size-4 accent-accent"
              checked={profile === 'quick'}
              onChange={() => setProfile('quick')}
            />
            <span>
              <strong>Quick</strong> — metadata of every file + duplicates (target: 2 TB HDD under 15 min)
            </span>
          </label>
          <label className="flex items-start gap-2 text-sm">
            <input
              type="radio"
              name="profile"
              className="mt-0.5 size-4 accent-accent"
              checked={profile === 'deep'}
              onChange={() => setProfile('deep')}
            />
            <span>
              <strong>Deep</strong> — also full BLAKE3 hash of every file (reads every byte: a 2 TB HDD takes 3–5 hours)
            </span>
          </label>
          <CheckField
            label="Also SHA-256 (Deep only)"
            checked={profile === 'deep' && sha256}
            disabled={profile !== 'deep'}
            onChange={setSha256}
          />
        </fieldset>

        <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
          <InlineNumber
            before="Skip duplicate check for files smaller than"
            after="KB"
            value={kbText}
            onChange={setMinKb}
          />
          <InlineNumber before="Time budget" after="min" value={minText} onChange={setBudgetMin} />
          <Button type="submit" variant="primary" busy={start.isPending} busyLabel="Starting…" disabled={!canStart}>
            Start scan
          </Button>
        </div>

        {scanning && <p className="text-sm text-muted">A scan is already running — see the Progress tab.</p>}
        {!scanning && selected.length === 0 && <p className="text-sm text-muted">Tick at least one drive first.</p>}
        {(kb === null || minutes === null) && (
          <p className="text-sm text-danger">Enter a number (0 or more) in both boxes.</p>
        )}
        <ErrorText error={start.error ?? settingsQ.error} />
      </form>
    </Panel>
  );
}
