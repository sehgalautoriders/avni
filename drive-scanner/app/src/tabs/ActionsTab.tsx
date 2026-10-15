import { useState } from 'react';
import { ACTION_LABEL, type ActionKind, type ActionResult } from '../api';
import { ActionOutcome, ActionRunning } from '../components/ActionLive';
import { ActionPreviewModal } from '../components/ActionPreviewModal';
import { VerdictByDrive } from '../components/VerdictTables';
import {
  Button,
  ErrorText,
  Loading,
  Panel,
  SectionTitle,
  Size,
  TableFrame,
  TickBox,
  DateText,
} from '../components/ui';
import { formatCount, formatDuration } from '../lib/format';
import { ACTION_INFO, ACTION_ORDER } from '../lib/labels';
import { useHistory, useSummary } from '../lib/queries';
import { useApp } from '../store';

function DriveFilter({ drives }: { drives: { id: string; label: string }[] }) {
  const chosen = useApp((s) => s.actionDrives);
  const setChosen = useApp((s) => s.setActionDrives);
  const valid = chosen.filter((id) => drives.some((d) => d.id === id));
  const toggle = (id: string): void => setChosen(valid.includes(id) ? valid.filter((x) => x !== id) : [...valid, id]);
  return (
    <fieldset className="shrink-0 rounded-md border border-line bg-surface px-3 py-2">
      <legend className="px-1 text-sm font-bold">Which drives?</legend>
      {drives.length === 0 ? (
        <p className="text-sm text-muted">Run a scan first. The drives come from the last scan.</p>
      ) : (
        <div className="flex flex-wrap items-center gap-x-5 gap-y-1">
          {drives.map((d) => (
            <label key={d.id} className="inline-flex items-center gap-2 text-sm">
              <TickBox checked={valid.includes(d.id)} onChange={() => toggle(d.id)} />
              <span>
                <strong>{d.id}</strong> {d.label}
              </span>
            </label>
          ))}
          <span className="text-xs text-muted">Nothing ticked = all scanned drives.</span>
        </div>
      )}
    </fieldset>
  );
}

function ActionTable({ onPreview, disabled }: { onPreview: (kind: ActionKind) => void; disabled: boolean }) {
  return (
    <TableFrame label="Actions" fit>
      <table className="tbl">
        <thead>
          <tr>
            <th>Action</th>
            <th>What it does</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {ACTION_ORDER.map((kind) => (
            <tr key={kind}>
              <td className="font-semibold">{ACTION_LABEL[kind]}</td>
              <td className="min-w-56">{ACTION_INFO[kind]}</td>
              <td>
                <Button small disabled={disabled} onClick={() => onPreview(kind)}>
                  Preview
                </Button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </TableFrame>
  );
}

function HistoryTable({ rows }: { rows: ActionResult[] }) {
  const showFinished = useApp((s) => s.showFinished);
  const openPreview = useApp((s) => s.openPreview);
  const latestFirst = [...rows].sort((a, b) => b.started_ms - a.started_ms);
  return (
    <TableFrame label="Past actions" fit className="max-h-80">
      <table className="tbl">
        <thead>
          <tr>
            <th>When</th>
            <th>Action</th>
            <th className="num">Done</th>
            <th className="num">Failed</th>
            <th className="num">Size</th>
            <th className="num">Time</th>
            <th>Log</th>
          </tr>
        </thead>
        <tbody>
          {latestFirst.length === 0 ? (
            <tr>
              <td colSpan={7} className="text-muted">
                Nothing has been run yet.
              </td>
            </tr>
          ) : (
            latestFirst.map((r) => (
              <tr key={`${r.started_ms}-${r.kind}`}>
                <td>
                  <DateText ms={r.started_ms} />
                </td>
                <td>{ACTION_LABEL[r.kind]}</td>
                <td className="num">{formatCount(r.done)}</td>
                <td className={`num ${r.failed > 0 ? 'font-bold text-danger' : ''}`}>{formatCount(r.failed)}</td>
                <td className="num">
                  <Size bytes={r.bytes} />
                </td>
                <td className="num">{formatDuration(r.ms)}</td>
                <td>
                  <div className="flex flex-wrap gap-2">
                    <Button small onClick={() => showFinished(r.log_path || null)}>
                      Finished files
                    </Button>
                    {r.log_path && (
                      <Button small onClick={() => openPreview(r.log_path, { table: true })}>
                        View log
                      </Button>
                    )}
                  </div>
                </td>
              </tr>
            ))
          )}
        </tbody>
      </table>
    </TableFrame>
  );
}

export function ActionsTab() {
  const summaryQ = useSummary();
  const historyQ = useHistory();
  const running = useApp((s) => s.actionRunning);
  const progress = useApp((s) => s.actionProgress);
  const result = useApp((s) => s.actionResult);
  const clearResult = useApp((s) => s.clearActionResult);
  const chosen = useApp((s) => s.actionDrives);
  const [previewKind, setPreviewKind] = useState<ActionKind | null>(null);

  const summary = summaryQ.data;
  const scanned = summary ? summary.drives.map((d) => ({ id: d.drive.id, label: d.drive.label })) : [];
  const drives = chosen.filter((id) => scanned.some((d) => d.id === id));

  return (
    <Panel label="Plan and actions">
      <DriveFilter drives={scanned} />
      <ErrorText error={summaryQ.error} />

      <div className="grid min-h-0 gap-3 lg:flex-1 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] lg:grid-rows-[minmax(0,1fr)] lg:overflow-hidden">
        <div className="flex min-h-0 flex-col gap-3 lg:overflow-y-auto">
          {running && <ActionRunning progress={progress} />}
          {!running && result && <ActionOutcome result={result} onDismiss={clearResult} />}
          <ActionTable onPreview={setPreviewKind} disabled={running} />
          {running && <p className="text-sm text-muted">Another action is running. Wait for it to finish, or cancel it.</p>}
        </div>

        <div className="flex min-h-0 flex-col gap-3 lg:overflow-y-auto">
          <SectionTitle>What was found</SectionTitle>
          {summaryQ.isPending ? (
            <Loading />
          ) : summary ? (
            <VerdictByDrive summary={summary} />
          ) : (
            <p className="rounded-md border border-line bg-surface p-3 text-sm font-medium">
              No scan yet — go to Drives and press Start scan
            </p>
          )}
          <SectionTitle>Past actions (latest first)</SectionTitle>
          <ErrorText error={historyQ.error} />
          {historyQ.isPending ? <Loading /> : <HistoryTable rows={historyQ.data ?? []} />}
        </div>
      </div>

      {previewKind && (
        <ActionPreviewModal kind={previewKind} drives={drives} onClose={() => setPreviewKind(null)} />
      )}
    </Panel>
  );
}
