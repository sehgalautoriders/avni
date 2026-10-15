import { VERDICT_LABEL, type DriveSummary, type Summary, type Verdict } from '../api';
import { ErrorText, Loading, NoScanYet, Panel, Size, TableFrame } from '../components/ui';
import { formatBytes, formatCount, formatDate, formatDuration } from '../lib/format';
import { MOVABLE_VERDICTS, NEEDED_VERDICTS, PROFILE_LABEL, ROLE_LABEL, VERDICT_ORDER } from '../lib/labels';
import { useSummary } from '../lib/queries';
import { findTotal, hasUnjudged, sumTotals, type Amount } from '../lib/totals';

function Fact({ label, value, title }: { label: string; value: string; title?: string }) {
  return (
    <div className="flex flex-col" title={title}>
      <dt className="text-xs font-semibold uppercase tracking-wide text-muted">{label}</dt>
      <dd className="text-base font-semibold tabular-nums">{value}</dd>
    </div>
  );
}

function Facts({ s }: { s: Summary }) {
  return (
    <dl className="grid shrink-0 grid-cols-2 gap-x-4 gap-y-2 rounded-md border border-line bg-surface p-3 sm:grid-cols-3 lg:grid-cols-5">
      <Fact label="Scanned at" value={formatDate(s.scanned_at_ms)} />
      <Fact label="Scan type" value={PROFILE_LABEL[s.profile]} />
      <Fact label="Total time" value={formatDuration(s.total_ms)} />
      <Fact label="Metadata time" value={formatDuration(s.metadata_ms)} />
      <Fact label="Hash time" value={s.hash_ms > 0 ? formatDuration(s.hash_ms) : '—'} />
      <Fact label="Files" value={formatCount(s.files)} />
      <Fact label="Folders" value={formatCount(s.dirs)} />
      <Fact label="Total size" value={formatBytes(s.bytes)} title={`${formatCount(s.bytes)} bytes`} />
      <Fact
        label="Duplicate groups"
        value={`${formatCount(s.dup_groups_confirmed)} confirmed / ${formatCount(s.dup_groups)}`}
        title="Confirmed = checked with a full hash. The rest were matched by size and sample."
      />
      <Fact
        label="Space wasted by duplicates"
        value={formatBytes(s.dup_wasted_bytes)}
        title={`${formatCount(s.dup_wasted_bytes)} bytes`}
      />
    </dl>
  );
}

function sizeCell(total: Amount) {
  if (total.files === 0 && total.bytes === 0) {
    return (
      <span className="text-muted" title="0 files">
        —
      </span>
    );
  }
  return (
    <span title={`${formatCount(total.files)} files · ${formatCount(total.bytes)} bytes`}>
      {formatBytes(total.bytes)}
    </span>
  );
}

function SystemDriveCallout({ d }: { d: DriveSummary }) {
  const needed = sumTotals(d.totals, NEEDED_VERDICTS);
  const movable = sumTotals(d.totals, MOVABLE_VERDICTS);
  return (
    <div className="grid shrink-0 gap-3 sm:grid-cols-2" aria-label={`What is on ${d.drive.id}`}>
      <div
        className="rounded-md border-2 border-ok bg-ok-bg px-4 py-2"
        title="Keep + Protected files: the system, programs and anything you told the app never to touch."
      >
        <div className="text-sm font-semibold text-ok">Needed by Windows/apps (Keep)</div>
        <div className="text-2xl font-bold tabular-nums text-ok">{formatBytes(needed.bytes)}</div>
        <div className="text-xs text-ok">{formatCount(needed.files)} files on {d.drive.id}</div>
      </div>
      <div
        className="rounded-md border-2 border-accent bg-surface px-4 py-2"
        title="Transfer + Already on Google Drive + Duplicate + Junk + Extract + Processed + Review"
      >
        <div className="text-sm font-semibold text-accent-text">Can move off {d.drive.id}</div>
        <div className="text-2xl font-bold tabular-nums text-accent-text">{formatBytes(movable.bytes)}</div>
        <div className="text-xs text-muted">{formatCount(movable.files)} files on {d.drive.id}</div>
      </div>
    </div>
  );
}

function DriveRows({ d, columns }: { d: DriveSummary; columns: (Verdict | null)[] }) {
  return (
    <>
      <tr>
        <td className="font-semibold">{d.drive.id}</td>
        <td>{d.drive.label || <span className="text-muted">—</span>}</td>
        <td>{ROLE_LABEL[d.drive.role]}</td>
        <td>{d.method || '—'}</td>
        <td className="num">{formatCount(d.files)}</td>
        <td className="num">
          <Size bytes={d.bytes} />
        </td>
        {columns.map((v) => (
          <td key={v ?? 'none'} className="num">
            {sizeCell(findTotal(d.totals, v))}
          </td>
        ))}
      </tr>
      {d.error && (
        <tr>
          <td colSpan={6 + columns.length} className="text-xs font-semibold text-danger">
            Problem on {d.drive.id} — {d.error}
          </td>
        </tr>
      )}
    </>
  );
}

function DriveTable({ s }: { s: Summary }) {
  const columns: (Verdict | null)[] = [...VERDICT_ORDER];
  if (hasUnjudged(s.totals) || s.drives.some((d) => hasUnjudged(d.totals))) columns.push(null);
  return (
    <TableFrame label="What was found on each drive">
      <table className="tbl">
        <thead>
          <tr>
            <th>Drive</th>
            <th>Label</th>
            <th>Role</th>
            <th>Method</th>
            <th className="num">Files</th>
            <th className="num">Size</th>
            {columns.map((v) => (
              <th key={v ?? 'none'} className="num wrap">
                {v ? VERDICT_LABEL[v] : 'No verdict yet'}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {s.drives.map((d) => (
            <DriveRows key={d.drive.id} d={d} columns={columns} />
          ))}
        </tbody>
        <tfoot>
          <tr>
            <td>Total</td>
            <td />
            <td />
            <td />
            <td className="num">{formatCount(s.files)}</td>
            <td className="num">
              <Size bytes={s.bytes} />
            </td>
            {columns.map((v) => (
              <td key={v ?? 'none'} className="num">
                {sizeCell(findTotal(s.totals, v))}
              </td>
            ))}
          </tr>
        </tfoot>
      </table>
    </TableFrame>
  );
}

export function SummaryTab() {
  const q = useSummary();
  const s = q.data;
  const systemDrives = s ? s.drives.filter((d) => d.drive.role === 'system' || d.drive.id.toUpperCase() === 'C:') : [];
  return (
    <Panel label="Summary">
      <ErrorText error={q.error} />
      {q.isPending && <Loading text="Loading the summary…" />}
      {q.isSuccess && s === null && <NoScanYet />}
      {s && (
        <>
          <Facts s={s} />
          {systemDrives.map((d) => (
            <SystemDriveCallout key={d.drive.id} d={d} />
          ))}
          <DriveTable s={s} />
          {s.notes.length > 0 && (
            <section aria-label="Notes" className="max-h-32 shrink-0 overflow-auto rounded-md border border-line bg-surface p-3">
              <h2 className="mb-1 text-sm font-bold">Notes</h2>
              <ul className="list-disc space-y-0.5 pl-5 text-sm">
                {s.notes.map((n, i) => (
                  <li key={`${i}-${n}`}>{n}</li>
                ))}
              </ul>
            </section>
          )}
        </>
      )}
    </Panel>
  );
}
