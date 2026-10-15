import { useMemo, useState } from 'react';
import { keepPreviousData, useQuery } from '@tanstack/react-query';
import {
  ACTION_LABEL,
  api,
  type ActionKind,
  type FinishedQuery,
  type FinishedResult,
  type FinishedRow,
} from '../api';
import {
  DateText,
  ErrorText,
  Field,
  INPUT_CLASS,
  Loading,
  Pager,
  Panel,
  ResultPill,
  Size,
  Spinner,
  TableFrame,
  moveRowFocus,
} from '../components/ui';
import { useDebounced } from '../lib/hooks';
import { formatBytes, formatCount } from '../lib/format';
import { ACTION_ORDER } from '../lib/labels';
import { baseName } from '../lib/path';
import { qk } from '../lib/queries';
import { useApp } from '../store';

const PAGE_SIZE = 200;

const RESULT_OPTIONS: { value: FinishedResult; label: string }[] = [
  { value: 'ok', label: 'Done' },
  { value: 'skipped', label: 'Skipped' },
  { value: 'failed', label: 'Failed' },
];

const isAction = (v: string): v is ActionKind => (ACTION_ORDER as string[]).includes(v);
const isResult = (v: string): v is FinishedResult => RESULT_OPTIONS.some((o) => o.value === v);

/** Where the file can be looked at: the new place, the kept copy, or (skipped / failed / gone) the old place. */
function previewTarget(row: FinishedRow): string {
  if (row.result !== 'ok') return row.source;
  return row.now_at || row.kept_at || row.source;
}

/** The "Now at" text: the new path, or "Recycle Bin". */
function nowAtText(row: FinishedRow): string {
  if (row.now_at) return row.now_at;
  if (/recycle/i.test(row.target)) return 'Recycle Bin';
  return row.target || '—';
}

function FinishedRowView({ row, onOpen }: { row: FinishedRow; onOpen: (path: string) => void }) {
  const target = previewTarget(row);
  const nowAt = nowAtText(row);
  const gone = row.result === 'ok' && row.now_at !== '' && !row.exists;
  return (
    <tr
      className="clickable"
      tabIndex={-1}
      title="Click to see this file here"
      onClick={() => onOpen(target)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') onOpen(target);
      }}
    >
      <td>
        <DateText ms={row.when_ms} />
      </td>
      <td className="min-w-32">{ACTION_LABEL[row.action]}</td>
      <td>
        <ResultPill result={row.result} />
      </td>
      <td className="max-w-48 truncate font-medium" title={row.name}>
        {row.name}
      </td>
      <td className="max-w-44">
        <div className="truncate" title={nowAt}>
          {nowAt}
        </div>
        {gone && <div className="text-xs font-semibold text-danger">not there any more</div>}
      </td>
      <td className="max-w-44 truncate" title={row.source}>
        {row.source || <span className="text-muted">—</span>}
      </td>
      <td className="num">
        <Size bytes={row.bytes} />
      </td>
      <td className="max-w-60">
        <div className="truncate" title={row.message}>
          {row.message || <span className="text-muted">—</span>}
        </div>
        {row.kept_at && (
          <div className="flex gap-1 text-xs text-muted">
            <span className="shrink-0">Same file kept at:</span>
            <button
              type="button"
              title={row.kept_at}
              className="min-w-0 truncate rounded text-left font-medium underline hover:text-accent-text"
              onClick={(e) => {
                e.stopPropagation();
                onOpen(row.kept_at);
              }}
            >
              {row.kept_at}
            </button>
          </div>
        )}
      </td>
    </tr>
  );
}

export function FinishedTab() {
  const openPreview = useApp((s) => s.openPreview);
  const logPath = useApp((s) => s.finishedLog);
  const clearLog = useApp((s) => s.clearFinishedLog);

  const [action, setAction] = useState<ActionKind | ''>('');
  const [result, setResult] = useState<FinishedResult | ''>('');
  const [searchText, setSearchText] = useState('');
  const search = useDebounced(searchText, 300).trim();

  // Any change of filter (or of "this run only") starts again at the first page.
  const scope = `${action}|${result}|${search}|${logPath ?? ''}`;
  const [position, setPosition] = useState({ scope, offset: 0 });
  if (position.scope !== scope) setPosition({ scope, offset: 0 });
  const offset = position.scope === scope ? position.offset : 0;

  const query = useMemo<FinishedQuery>(
    () => ({
      action: action || null,
      result: result || null,
      search: search || null,
      log_path: logPath,
      offset,
      limit: PAGE_SIZE,
    }),
    [action, result, search, logPath, offset],
  );
  const finishedQ = useQuery({
    queryKey: [...qk.finished, query],
    queryFn: () => api.queryFinished(query),
    placeholderData: keepPreviousData,
  });
  const page = finishedQ.data;
  const filtered = action !== '' || result !== '' || search !== '' || logPath !== null;
  const updating = finishedQ.isFetching || searchText.trim() !== search;

  return (
    <Panel label="Finished files">
      <div className="flex shrink-0 flex-wrap items-end gap-x-4 gap-y-2">
        <Field label="What was done">
          {(id) => (
            <select
              id={id}
              className={INPUT_CLASS}
              value={action}
              onChange={(e) => setAction(isAction(e.target.value) ? e.target.value : '')}
            >
              <option value="">All</option>
              {ACTION_ORDER.map((a) => (
                <option key={a} value={a}>
                  {ACTION_LABEL[a]}
                </option>
              ))}
            </select>
          )}
        </Field>
        <Field label="Result">
          {(id) => (
            <select
              id={id}
              className={INPUT_CLASS}
              value={result}
              onChange={(e) => setResult(isResult(e.target.value) ? e.target.value : '')}
            >
              <option value="">All</option>
              {RESULT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          )}
        </Field>
        <Field label="Search" hint="file name, old place, new place or note" className="min-w-52 flex-1">
          {(id) => (
            <input
              id={id}
              type="search"
              className={INPUT_CLASS}
              value={searchText}
              onChange={(e) => setSearchText(e.target.value)}
            />
          )}
        </Field>
        {logPath !== null && (
          <span
            className="mb-1.5 inline-flex items-center gap-1 rounded-full border border-accent bg-surface-3 py-0.5 pl-3 pr-1 text-sm font-semibold"
            title={logPath}
          >
            This run only
            <button
              type="button"
              onClick={clearLog}
              aria-label="Show every run again"
              className="rounded-full px-2 text-base leading-none hover:bg-surface-2"
            >
              ×
            </button>
          </span>
        )}
      </div>

      <ErrorText error={finishedQ.error} />

      <TableFrame label="List of finished files" className={updating && page ? 'opacity-70' : ''} onKeyDown={moveRowFocus}>
        {!page ? (
          <div className="p-4">
            <Loading text="Loading the finished files…" />
          </div>
        ) : page.rows.length === 0 ? (
          <p className="p-4 text-sm font-medium">
            {filtered
              ? 'No finished files match these filters.'
              : 'Nothing finished yet — run an action in Plan & Actions.'}
          </p>
        ) : (
          <table className="tbl">
            <thead>
              <tr>
                <th>When</th>
                <th>Action</th>
                <th>Result</th>
                <th>File</th>
                <th>Now at</th>
                <th>Was at</th>
                <th className="num">Size</th>
                <th>Note</th>
              </tr>
            </thead>
            <tbody>
              {page.rows.map((row) => (
                <FinishedRowView key={`${row.id}-${row.log_path}`} row={row} onOpen={(p) => openPreview(p)} />
              ))}
            </tbody>
          </table>
        )}
      </TableFrame>

      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <Pager
          offset={offset}
          limit={PAGE_SIZE}
          total={page?.total ?? 0}
          busy={finishedQ.isFetching}
          onOffset={(o) => setPosition({ scope, offset: o })}
        />
        <p className="flex items-center gap-2 text-sm font-semibold tabular-nums" aria-live="polite">
          {updating && <Spinner />}
          {page ? (
            <>
              {formatCount(page.total)} files ·{' '}
              <span title={`${formatCount(page.bytes)} bytes`}>{formatBytes(page.bytes)}</span>
              {logPath !== null && <span className="font-normal text-muted"> · {baseName(logPath)}</span>}
            </>
          ) : (
            'Loading…'
          )}
        </p>
      </div>
    </Panel>
  );
}
