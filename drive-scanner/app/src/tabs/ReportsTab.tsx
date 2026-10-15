import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { api, type ReportFiles } from '../api';
import { PathButtons } from '../components/PathButtons';
import { Button, CheckField, DateText, ErrorText, Loading, Panel, SectionTitle, Size, TableFrame } from '../components/ui';
import { errMsg } from '../lib/errors';
import { formatCount, formatDuration } from '../lib/format';
import { qk, useReports } from '../lib/queries';
import { useApp } from '../store';

function PathLine({ label, path }: { label: string; path: string }) {
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <span className="font-semibold">{label}</span>
      <span className="break-all text-sm" title={path}>
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
        <PathLine label="Folder" path={r.folder} />
        <PathLine label="Excel" path={r.xlsx} />
        {r.csv.map((p) => (
          <PathLine key={p} label="CSV" path={p} />
        ))}
      </ul>
    </section>
  );
}

export function ReportsTab() {
  const qc = useQueryClient();
  const reportsQ = useReports();
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
                      <PathButtons path={r.path} />
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
