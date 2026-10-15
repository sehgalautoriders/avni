import { useMemo, useState, type KeyboardEvent } from 'react';
import { keepPreviousData, useMutation, useQuery } from '@tanstack/react-query';
import { api, VERDICT_LABEL, type FileQuery, type FileRow, type SortKey, type Verdict } from '../api';
import {
  CheckField,
  ErrorText,
  Field,
  INPUT_CLASS,
  Loading,
  NoScanYet,
  Pager,
  Panel,
  Size,
  SortTh,
  Spinner,
  TableFrame,
  VerdictPill,
  DateText,
} from '../components/ui';
import { useDebounced } from '../lib/hooks';
import { formatBytes, formatCount, shortHash } from '../lib/format';
import { isVerdict, VERDICT_ORDER } from '../lib/labels';
import { qk, useSummary } from '../lib/queries';

const PAGE_SIZE = 200;

interface Filters {
  drive: string;
  verdict: Verdict | '';
  search: string;
  ext: string;
  minMb: string;
  includeDirs: boolean;
  sort: SortKey;
  desc: boolean;
  offset: number;
}

const INITIAL: Filters = {
  drive: '',
  verdict: '',
  search: '',
  ext: '',
  minMb: '',
  includeDirs: false,
  sort: 'size',
  desc: true,
  offset: 0,
};

/** Columns that start with the biggest / newest first when first clicked. */
const DESC_FIRST: SortKey[] = ['size', 'modified', 'created'];

function toQuery(f: Filters, search: string, ext: string, minMb: string): FileQuery {
  const mb = Number(minMb);
  return {
    drive: f.drive || null,
    verdict: f.verdict || null,
    search: search.trim() || null,
    ext: ext.trim().replace(/^\./, '') || null,
    min_size: minMb.trim() !== '' && Number.isFinite(mb) && mb > 0 ? Math.round(mb * 1024 * 1024) : null,
    include_dirs: f.includeDirs,
    sort: f.sort,
    desc: f.desc,
    offset: f.offset,
    limit: PAGE_SIZE,
  };
}

/** Arrow keys move between rows, so the table is one tab stop instead of 200. */
function moveRowFocus(e: KeyboardEvent<HTMLDivElement>): void {
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
  const down = e.key === 'ArrowDown';
  const row = e.target instanceof HTMLElement ? e.target.closest('tr') : null;
  const next = row
    ? down
      ? row.nextElementSibling
      : row.previousElementSibling
    : down
      ? e.currentTarget.querySelector('tbody tr')
      : null;
  if (next instanceof HTMLElement) {
    e.preventDefault();
    next.focus();
  }
}

function FileRowView({ row, onReveal }: { row: FileRow; onReveal: (path: string) => void }) {
  const hashTitle = [row.blake3 && `BLAKE3: ${row.blake3}`, row.sha256 && `SHA-256: ${row.sha256}`, row.sample && `Sample: ${row.sample}`]
    .filter(Boolean)
    .join('\n');
  return (
    <tr
      className="clickable"
      tabIndex={-1}
      title="Double-click (or press Enter) to show this in its folder"
      onDoubleClick={() => onReveal(row.path)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') onReveal(row.path);
      }}
    >
      <td>
        <VerdictPill verdict={row.verdict} />
      </td>
      <td className="max-w-40 truncate" title={row.name}>
        {row.name}
      </td>
      <td className="max-w-60 truncate" title={row.path}>
        {row.path}
      </td>
      <td className="num">{row.is_dir ? <span className="text-muted">—</span> : <Size bytes={row.size} />}</td>
      <td>
        <DateText ms={row.modified_ms} />
      </td>
      <td>
        <DateText ms={row.created_ms} />
      </td>
      <td>{row.is_dir ? <span className="text-muted">folder</span> : row.ext || <span className="text-muted">—</span>}</td>
      <td className="max-w-40 truncate" title={row.reason}>
        {row.reason}
      </td>
      <td className="font-mono text-xs" title={hashTitle || undefined}>
        {shortHash(row.blake3, row.sample)}
      </td>
    </tr>
  );
}

export function FilesTab() {
  const summaryQ = useSummary();
  const [f, setF] = useState<Filters>(INITIAL);
  const search = useDebounced(f.search, 300);
  const ext = useDebounced(f.ext, 300);
  const minMb = useDebounced(f.minMb, 300);

  const patch = (p: Partial<Filters>): void => setF((prev) => ({ ...prev, offset: 0, ...p }));
  const onSort = (field: SortKey): void =>
    setF((prev) =>
      prev.sort === field
        ? { ...prev, desc: !prev.desc, offset: 0 }
        : { ...prev, sort: field, desc: DESC_FIRST.includes(field), offset: 0 },
    );

  const query = useMemo(() => toQuery(f, search, ext, minMb), [f, search, ext, minMb]);
  const filesQ = useQuery({
    queryKey: [...qk.files, query],
    queryFn: () => api.queryFiles(query),
    placeholderData: keepPreviousData,
    enabled: !!summaryQ.data,
  });
  const reveal = useMutation({ mutationFn: (path: string) => api.revealPath(path) });

  const page = filesQ.data;
  const typing = f.search !== search || f.ext !== ext || f.minMb !== minMb;
  const updating = filesQ.isFetching || typing;

  if (summaryQ.isPending) {
    return (
      <Panel label="Files">
        <Loading />
      </Panel>
    );
  }
  if (!summaryQ.data) {
    return (
      <Panel label="Files">
        <ErrorText error={summaryQ.error} />
        <NoScanYet />
      </Panel>
    );
  }
  const drives = summaryQ.data.drives;

  return (
    <Panel label="Files">
      <div className="flex shrink-0 flex-wrap items-end gap-x-4 gap-y-2">
        <Field label="Drive">
          {(id) => (
            <select id={id} className={INPUT_CLASS} value={f.drive} onChange={(e) => patch({ drive: e.target.value })}>
              <option value="">All</option>
              {drives.map((d) => (
                <option key={d.drive.id} value={d.drive.id}>
                  {d.drive.id} {d.drive.label}
                </option>
              ))}
            </select>
          )}
        </Field>
        <Field label="What to do with it">
          {(id) => (
            <select
              id={id}
              className={INPUT_CLASS}
              value={f.verdict}
              onChange={(e) => patch({ verdict: isVerdict(e.target.value) ? e.target.value : '' })}
            >
              <option value="">All</option>
              {VERDICT_ORDER.map((v) => (
                <option key={v} value={v}>
                  {VERDICT_LABEL[v]}
                </option>
              ))}
            </select>
          )}
        </Field>
        <Field label="Search" hint={'name, or a path with \\'} className="min-w-52 flex-1">
          {(id) => (
            <input
              id={id}
              type="search"
              className={INPUT_CLASS}
              value={f.search}
              onChange={(e) => patch({ search: e.target.value })}
            />
          )}
        </Field>
        <Field label="Extension">
          {(id) => (
            <input
              id={id}
              className={`${INPUT_CLASS} w-24`}
              placeholder="pdf"
              value={f.ext}
              onChange={(e) => patch({ ext: e.target.value })}
            />
          )}
        </Field>
        <Field label="Smallest size (MB)">
          {(id) => (
            <input
              id={id}
              type="number"
              min={0}
              step="any"
              inputMode="decimal"
              className={`${INPUT_CLASS} w-28`}
              value={f.minMb}
              onChange={(e) => patch({ minMb: e.target.value })}
            />
          )}
        </Field>
        <div className="pb-1.5">
          <CheckField label="Include folders" checked={f.includeDirs} onChange={(v) => patch({ includeDirs: v })} />
        </div>
      </div>

      <ErrorText error={filesQ.error ?? reveal.error} />

      <TableFrame label="List of files" className={updating && page ? 'opacity-70' : ''} onKeyDown={moveRowFocus}>
        {!page ? (
          <div className="p-4">
            <Loading text="Loading files…" />
          </div>
        ) : (
          <table className="tbl">
            <thead>
              <tr>
                <SortTh label="Verdict" field="verdict" sort={f.sort} desc={f.desc} onSort={onSort} />
                <SortTh label="Name" field="name" sort={f.sort} desc={f.desc} onSort={onSort} />
                <SortTh label="Path" field="path" sort={f.sort} desc={f.desc} onSort={onSort} />
                <SortTh label="Size" field="size" sort={f.sort} desc={f.desc} onSort={onSort} numeric />
                <SortTh label="Modified" field="modified" sort={f.sort} desc={f.desc} onSort={onSort} />
                <SortTh label="Created" field="created" sort={f.sort} desc={f.desc} onSort={onSort} />
                <SortTh label="Ext" field="ext" sort={f.sort} desc={f.desc} onSort={onSort} />
                <th>Reason</th>
                <th title="First 12 characters of the BLAKE3 hash, or sample: for a quick sample">Hash</th>
              </tr>
            </thead>
            <tbody>
              {page.rows.length === 0 ? (
                <tr>
                  <td colSpan={9} className="text-muted">
                    No files match these filters.
                  </td>
                </tr>
              ) : (
                page.rows.map((row) => <FileRowView key={row.id} row={row} onReveal={reveal.mutate} />)
              )}
            </tbody>
          </table>
        )}
      </TableFrame>

      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <Pager
          offset={f.offset}
          limit={PAGE_SIZE}
          total={page?.total ?? 0}
          busy={filesQ.isFetching}
          onOffset={(offset) => setF((prev) => ({ ...prev, offset }))}
        />
        <p className="flex items-center gap-2 text-sm font-semibold tabular-nums" aria-live="polite">
          {updating && <Spinner />}
          {page ? (
            <>
              {formatCount(page.total)} files · <span title={`${formatCount(page.bytes)} bytes`}>{formatBytes(page.bytes)}</span> in this filter
            </>
          ) : (
            'Loading…'
          )}
        </p>
      </div>
    </Panel>
  );
}
