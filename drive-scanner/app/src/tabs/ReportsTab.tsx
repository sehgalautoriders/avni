import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { api, type ReportFiles } from '../api';
import { PathButtons } from '../components/PathButtons';
import { Button, CheckField, DateText, ErrorText, Loading, Panel, SectionTitle, Size, TableFrame } from '../components/ui';
import { errMsg } from '../lib/errors';
import { formatCount, formatDuration } from '../lib/format';
import { baseName, isTableName } from '../lib/path';
import { qk, useReports } from '../lib/queries';
import { useApp } from '../store';

/** "Avni Drive Scan 2026-10-15 - C.csv" -> "View CSV C:". Falls back to "View CSV". */
function csvViewLabel(path: string): string {
  const part = /- ([^-]+?)(?: \(\d+\))?\.csv$/i.exec(baseName(path))?.[1]?.trim();
  if (!part) return 'View CSV';
  return `View CSV ${part.length === 1 ? `${part.toUpperCase()}:` : part}`;
}

/** One file of the result: a primary View button (when it can be viewed here), its name, and the small Windows buttons. */
function ResultLine({ viewLabel, path, table = true }: { viewLabel: string; path: string; table?: boolean }) {
  const openPreview = useApp((s) => s.openPreview);
  return (
    <li className="flex flex-wrap items-start gap-x-3 gap-y-1">
      <Button small variant="primary" onClick={() => openPreview(path, { table })}>
        {viewLabel}
      </Button>
      <span className="min-w-0 flex-1 basis-56 break-all pt-1 text-sm" title={path}>
        {path}
      </span>
      <PathButtons path={path} />
    </li>
  );
}

function ReportResult({ r }: { r: ReportFiles }) {
  return (
    <section aria-label="Files that were just written" className="flex shrink-0 flex-col gap-2 rounded-md border-2 border-ok bg-surface p-3">
      <SectionTitle>
        Done — {formatCount(r.rows)} rows in {formatDuration(r.ms)}
      </SectionTitle>
      <ul className="flex flex-col gap-2">
        <ResultLine viewLabel="View Excel" path={r.xlsx} />
        {r.csv.map((p) => (
          <ResultLine key={p} viewLabel={csvViewLabel(p)} path={p} />
        ))}
      </ul>
      <p className="break-all text-xs text-muted">
        Saved in {r.folder} — you do not need to look for it: use the View buttons above.
      </p>
    </section>
  );
}

export function ReportsTab() {
  const qc = useQueryClient();
  const reportsQ = useReports();
  const openPreview = useApp((s) => s.openPreview);
  const busy = useApp((s) => s.reportBusy);
  const messages = useApp((s) => s.reportMessages);
  const result = useApp((s) => s.reportResult);
  const error = useApp((s) => s.reportError);
  const [includeAll, setIncludeAll] = useState(true);

  const onExport = async (): Promise<void> => {
    const app = useApp.getState();
    app.beginReport();
    try {
      const files = await api.exportReports(includeAll);
      useApp.getState().finishReport(files, null);
      void qc.invalidateQueries({ queryKey: qk.reports });
      void qc.invalidateQueries({ queryKey: qk.table });
    } catch (e) {
      useApp.getState().finishReport(null, errMsg(e));
    }
  };

  const latestFirst = [...(reportsQ.data ?? [])].sort((a, b) => b.modified_ms - a.modified_ms);
  const lastMessage = messages[messages.length - 1];

  return (
    <Panel label="Reports">
      <div className="flex shrink-0 flex-wrap items-center gap-x-5 gap-y-2">
        <Button variant="primary" busy={busy} busyLabel="Exporting…" onClick={() => void onExport()}>
          Export Excel + CSV
        </Button>
        <CheckField label="Put every file in Excel too (large)" checked={includeAll} onChange={setIncludeAll} />
      </div>

      {busy && (
        <div className="shrink-0 rounded-md border border-line bg-surface p-3" aria-live="polite">
          <p className="font-semibold">{lastMessage ?? 'Starting…'}</p>
          {messages.length > 1 && (
            <ul className="mt-1 max-h-24 overflow-auto text-sm text-muted">
              {messages.slice(0, -1).map((m, i) => (
                <li key={`${i}-${m}`}>{m}</li>
              ))}
            </ul>
          )}
        </div>
      )}
      <ErrorText error={error} />
      {result && !busy && <ReportResult r={result} />}

      <SectionTitle>Reports on this computer (latest first)</SectionTitle>
      <ErrorText error={reportsQ.error} />
      <TableFrame label="Saved reports">
        {reportsQ.isPending ? (
          <div className="p-4">
            <Loading />
          </div>
        ) : (
          <table className="tbl">
            <thead>
              <tr>
                <th>Name</th>
                <th className="num">Size</th>
                <th>Modified</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {latestFirst.length === 0 ? (
                <tr>
                  <td colSpan={4} className="text-muted">
                    No reports yet. Press Export Excel + CSV.
                  </td>
                </tr>
              ) : (
                latestFirst.map((r) => (
                  <tr key={r.path}>
                    <td className="break-all font-medium" title={r.path}>
                      {r.name}
                    </td>
                    <td className="num">
                      <Size bytes={r.bytes} />
                    </td>
                    <td>
                      <DateText ms={r.modified_ms} />
                    </td>
                    <td>
                      <div className="flex flex-wrap items-start gap-2">
                        <Button
                          small
                          variant="primary"
                          onClick={() => openPreview(r.path, { table: isTableName(r.name) })}
                        >
                          View
                        </Button>
                        <PathButtons path={r.path} />
                      </div>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        )}
      </TableFrame>
    </Panel>
  );
}
