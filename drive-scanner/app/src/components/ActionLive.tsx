import { useMutation } from '@tanstack/react-query';
import { ACTION_LABEL, api, type ActionProgress, type ActionResult } from '../api';
import { formatCount, formatDate, formatDuration } from '../lib/format';
import { PathButtons } from './PathButtons';
import { Button, ErrorText, Notice, SectionTitle, Size, Spinner } from './ui';

/** Live progress of the action that is running right now. */
export function ActionRunning({ progress }: { progress: ActionProgress | null }) {
  const cancel = useMutation({ mutationFn: () => api.cancel() });
  const percent =
    progress && progress.total > 0 ? Math.min(100, (progress.done / progress.total) * 100) : 0;
  return (
    <section aria-label="Action in progress" className="flex flex-col gap-2 rounded-md border-2 border-accent bg-surface p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <SectionTitle>
          <span className="flex items-center gap-2">
            <Spinner /> {progress?.kind ? ACTION_LABEL[progress.kind] : 'Starting…'}
          </span>
        </SectionTitle>
        <Button variant="danger" small busy={cancel.isPending} busyLabel="Cancelling…" onClick={() => cancel.mutate()}>
          Cancel
        </Button>
      </div>
      <ErrorText error={cancel.error} />
      {progress && (
        <>
          <div className="flex flex-wrap justify-between gap-2 text-sm tabular-nums">
            <span>
              {formatCount(progress.done)} of {formatCount(progress.total)} done
            </span>
            <span>
              <Size bytes={progress.bytes_done} /> of <Size bytes={progress.bytes_total} />
            </span>
            <span className={progress.failed > 0 ? 'font-bold text-danger' : ''}>
              {formatCount(progress.failed)} failed
            </span>
          </div>
          <div
            role="progressbar"
            aria-label="Action progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(percent)}
            className="h-3 overflow-hidden rounded-full border border-line bg-surface-2"
          >
            <div className="h-full bg-accent transition-[width] duration-500" style={{ width: `${percent}%` }} />
          </div>
          {progress.current && (
            <p className="truncate text-sm" title={progress.current}>
              <span className="font-semibold">Now: </span>
              {progress.current}
            </p>
          )}
          {progress.message && (
            <p className="text-sm text-muted" aria-live="polite">
              {progress.message}
            </p>
          )}
        </>
      )}
    </section>
  );
}

/** What the last action did. */
export function ActionOutcome({ result, onDismiss }: { result: ActionResult; onDismiss: () => void }) {
  return (
    <section
      aria-label="Result of the last action"
      className="flex flex-col gap-2 rounded-md border border-line bg-surface p-3"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <SectionTitle>Finished: {ACTION_LABEL[result.kind]}</SectionTitle>
        <Button small variant="ghost" onClick={onDismiss}>
          Dismiss
        </Button>
      </div>
      <dl className="grid grid-cols-2 gap-3 sm:grid-cols-5">
        {[
          ['Done', formatCount(result.done)],
          ['Failed', formatCount(result.failed)],
          ['Skipped', formatCount(result.skipped)],
          ['Size', null],
          ['Time', formatDuration(result.ms)],
        ].map(([label, value]) => (
          <div key={label}>
            <dt className="text-xs font-semibold uppercase tracking-wide text-muted">{label}</dt>
            <dd className={`text-base font-bold tabular-nums ${label === 'Failed' && result.failed > 0 ? 'text-danger' : ''}`}>
              {value ?? <Size bytes={result.bytes} />}
            </dd>
          </div>
        ))}
      </dl>
      {result.failed > 0 && (
        <Notice kind="warn">Some items failed. They were left where they were. Details are in the log.</Notice>
      )}
      {result.messages.length > 0 && (
        <ul className="max-h-28 list-disc space-y-0.5 overflow-auto pl-5 text-sm">
          {result.messages.map((m, i) => (
            <li key={`${i}-${m}`}>{m}</li>
          ))}
        </ul>
      )}
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <span className="text-muted">Started {formatDate(result.started_ms)}</span>
        {result.log_path && (
          <>
            <span className="break-all" title={result.log_path}>
              Log: {result.log_path}
            </span>
            <PathButtons path={result.log_path} openLabel="Open log" showFolder={false} />
          </>
        )}
      </div>
    </section>
  );
}
