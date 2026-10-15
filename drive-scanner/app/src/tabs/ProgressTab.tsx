import { useMutation } from '@tanstack/react-query';
import { api, type Phase, type ScanProgress } from '../api';
import { Button, ErrorText, Notice, Panel, SectionTitle, Size, TableFrame } from '../components/ui';
import { formatCount, formatDuration } from '../lib/format';
import { PHASE_STEPS } from '../lib/labels';
import { useSummary } from '../lib/queries';
import { isScanActive, useApp } from '../store';

function PhaseStrip({ phase }: { phase: Phase }) {
  const current = PHASE_STEPS.findIndex((s) => s.phase === phase);
  return (
    <ol className="flex flex-wrap items-center gap-2" aria-label="Scan steps">
      {PHASE_STEPS.map((step, i) => {
        const state = phase === 'done' || i < current ? 'done' : i === current ? 'current' : 'todo';
        const style =
          state === 'current'
            ? 'border-transparent bg-accent font-bold text-[#111418]'
            : state === 'done'
              ? 'border-ok bg-ok-bg text-ok font-semibold'
              : 'border-line bg-surface text-muted';
        return (
          <li
            key={step.phase}
            aria-current={state === 'current' ? 'step' : undefined}
            className={`flex items-center gap-1.5 rounded-full border px-3 py-1 text-sm ${style}`}
          >
            <span aria-hidden="true">{state === 'done' ? '✓' : i + 1}</span>
            {step.label}
          </li>
        );
      })}
    </ol>
  );
}

function Bar({ label, percent, detail }: { label: string; percent: number; detail: string }) {
  const value = Math.max(0, Math.min(100, percent));
  return (
    <div className="flex flex-col gap-1">
      <div className="flex flex-wrap justify-between gap-2 text-sm">
        <span className="font-semibold">{label}</span>
        <span className="tabular-nums">{detail}</span>
      </div>
      <div
        role="progressbar"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(value)}
        className="h-3 overflow-hidden rounded-full border border-line bg-surface-2"
      >
        <div className="h-full bg-accent transition-[width] duration-500" style={{ width: `${value}%` }} />
      </div>
    </div>
  );
}

function HashProgress({ p }: { p: ScanProgress }) {
  const percent =
    p.hash_bytes_total > 0
      ? (p.hash_bytes_done / p.hash_bytes_total) * 100
      : p.hash_files_total > 0
        ? (p.hash_files_done / p.hash_files_total) * 100
        : 0;
  return (
    <div className="flex flex-col gap-2 rounded-md border border-line bg-surface p-3">
      <Bar
        label="Hashing"
        percent={percent}
        detail={`${formatCount(p.hash_files_done)} of ${formatCount(p.hash_files_total)} files`}
      />
      <p className="text-sm tabular-nums">
        <Size bytes={p.hash_bytes_done} /> of <Size bytes={p.hash_bytes_total} /> read
      </p>
    </div>
  );
}

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col">
      <dt className="text-xs font-semibold uppercase tracking-wide text-muted">{label}</dt>
      <dd className="text-base font-semibold tabular-nums">{value}</dd>
    </div>
  );
}

export function ProgressTab() {
  const scan = useApp((s) => s.scan);
  const setTab = useApp((s) => s.setTab);
  const summaryQ = useSummary();
  const cancel = useMutation({ mutationFn: () => api.cancel() });

  const active = isScanActive(scan);
  const phase = scan?.phase ?? 'idle';

  if (!scan || phase === 'idle') {
    return (
      <Panel label="Progress">
        <div className="rounded-md border border-line bg-surface p-6 text-center text-base font-medium">
          No scan is running. Go to Drives and press Start scan.
        </div>
      </Panel>
    );
  }

  const finishedIn = summaryQ.data?.total_ms ?? scan.elapsed_ms;
  return (
    <Panel label="Progress">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <SectionTitle>{active ? 'Scan is running' : 'Scan'}</SectionTitle>
        {active && (
          <Button variant="danger" busy={cancel.isPending} busyLabel="Cancelling…" onClick={() => cancel.mutate()}>
            Cancel scan
          </Button>
        )}
      </div>
      <ErrorText error={cancel.error} />

      {phase === 'done' && (
        <Notice kind="ok">
          <span className="flex flex-wrap items-center gap-3">
            <span>Scan finished in {formatDuration(finishedIn)}.</span>
            <Button small variant="primary" onClick={() => setTab('summary')}>
              Open Summary
            </Button>
          </span>
        </Notice>
      )}
      {phase === 'cancelled' && <Notice kind="warn">Scan was cancelled. {scan.message}</Notice>}
      {phase === 'failed' && <Notice kind="danger">Scan failed. {scan.message}</Notice>}

      <PhaseStrip phase={phase} />

      <dl className="grid grid-cols-2 gap-3 rounded-md border border-line bg-surface p-3 sm:grid-cols-3">
        <Fact label="Time so far" value={formatDuration(scan.elapsed_ms)} />
        <Fact
          label="Time left (estimate)"
          value={scan.eta_ms !== null ? formatDuration(scan.eta_ms) : active ? 'working it out…' : '—'}
        />
        <div className="col-span-2 flex flex-col sm:col-span-1">
          <dt className="text-xs font-semibold uppercase tracking-wide text-muted">What it is doing</dt>
          <dd className="text-sm font-medium" aria-live="polite">
            {scan.message || '—'}
          </dd>
        </div>
      </dl>

      {(phase === 'hashing' || scan.hash_files_total > 0) && <HashProgress p={scan} />}

      <TableFrame label="Progress of each drive">
        <table className="tbl">
          <thead>
            <tr>
              <th>Drive</th>
              <th>Method</th>
              <th className="num">Files</th>
              <th className="num">Folders</th>
              <th className="num">Size</th>
              <th className="num">Time</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {scan.drives.length === 0 ? (
              <tr>
                <td colSpan={7} className="text-muted">
                  Waiting for the first drive…
                </td>
              </tr>
            ) : (
              scan.drives.map((d) => (
                <tr key={d.drive}>
                  <td className="font-semibold">{d.drive}</td>
                  <td>{d.method || '—'}</td>
                  <td className="num">{formatCount(d.files)}</td>
                  <td className="num">{formatCount(d.dirs)}</td>
                  <td className="num">
                    <Size bytes={d.bytes} />
                  </td>
                  <td className="num">{formatDuration(d.ms)}</td>
                  <td>
                    {d.error ? (
                      <span className="font-semibold text-danger">Problem: {d.error}</span>
                    ) : d.done ? (
                      <span className="font-semibold text-ok">Done</span>
                    ) : (
                      <span>Scanning…</span>
                    )}
                  </td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </TableFrame>
    </Panel>
  );
}
