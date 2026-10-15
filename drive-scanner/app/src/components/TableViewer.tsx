import { useState } from 'react';
import { keepPreviousData, useQuery } from '@tanstack/react-query';
import { api } from '../api';
import { useDebounced } from '../lib/hooks';
import { qk } from '../lib/queries';
import { ErrorText, INPUT_CLASS, Loading, Notice, Pager, Spinner, TableFrame } from './ui';

const PAGE_SIZE = 200;
/** Up to this many sheets show as tabs; more than that become a drop-down list. */
const MAX_SHEET_TABS = 8;

function SheetPicker({
  sheets,
  active,
  onPick,
}: {
  sheets: string[];
  active: string;
  onPick: (sheet: string) => void;
}) {
  if (sheets.length > MAX_SHEET_TABS) {
    return (
      <label className="flex items-center gap-2 text-sm">
        <span className="font-semibold">Sheet</span>
        <select className={INPUT_CLASS} value={active} onChange={(e) => onPick(e.target.value)}>
          {sheets.map((s) => (
            <option key={s} value={s}>
              {s}
            </option>
          ))}
        </select>
      </label>
    );
  }
  return (
    <nav aria-label="Sheets" className="flex flex-wrap gap-1">
      {sheets.map((s) => (
        <button
          key={s}
          type="button"
          aria-pressed={s === active}
          onClick={() => onPick(s)}
          className={`shrink-0 whitespace-nowrap rounded-md border px-3 py-1 text-sm ${
            s === active
              ? 'border-transparent bg-accent font-bold text-[#111418]'
              : 'border-line bg-surface hover:bg-surface-3'
          }`}
        >
          {s}
        </button>
      ))}
    </nav>
  );
}

/** A CSV report, an action log or an Excel sheet, shown as a table inside the app. */
export function TableViewer({ path }: { path: string }) {
  const [sheet, setSheet] = useState<string | null>(null);
  const [searchText, setSearchText] = useState('');
  const search = useDebounced(searchText, 400).trim();

  // Any change of sheet or search starts again at the first page.
  const scope = `${sheet ?? ''}\u0000${search}`;
  const [position, setPosition] = useState({ scope, offset: 0 });
  if (position.scope !== scope) setPosition({ scope, offset: 0 });
  const offset = position.scope === scope ? position.offset : 0;

  const q = useQuery({
    queryKey: [...qk.table, path, sheet, search, offset],
    queryFn: () => api.readTable({ path, sheet, search: search || null, offset, limit: PAGE_SIZE }),
    placeholderData: keepPreviousData,
  });
  const data = q.data;
  const active = sheet ?? data?.sheet ?? data?.sheets[0] ?? '';
  const columns = data ? Math.max(data.headers.length, ...data.rows.map((r) => r.length)) : 0;
  const waiting = q.isFetching || searchText.trim() !== search;

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      {data && data.sheets.length > 1 && (
        <SheetPicker sheets={data.sheets} active={active} onPick={(s) => setSheet(s)} />
      )}
      <div className="flex flex-wrap items-center gap-3">
        <label className="flex min-w-48 flex-1 items-center gap-2 text-sm">
          <span className="font-semibold">Search</span>
          <input
            type="search"
            className={`${INPUT_CLASS} min-w-0 flex-1`}
            value={searchText}
            onChange={(e) => setSearchText(e.target.value)}
            placeholder="type to find rows"
          />
        </label>
        {waiting && (
          <span className="flex items-center gap-2 text-sm text-muted" role="status">
            <Spinner /> Reading…
          </span>
        )}
      </div>

      <ErrorText error={q.error} />
      {data?.note && <Notice>{data.note}</Notice>}

      <TableFrame label="Table" className={waiting && data ? 'opacity-70' : ''}>
        {!data ? (
          <div className="p-4">
            <Loading text="Reading the file… big sheets can take a few seconds." />
          </div>
        ) : data.rows.length === 0 ? (
          <p className="p-4 text-sm font-medium">{search ? 'No rows match your search.' : 'This table has no rows.'}</p>
        ) : (
          <table className="tbl">
            <thead>
              <tr>
                <th className="num" scope="col">
                  #
                </th>
                {Array.from({ length: columns }, (_, i) => (
                  <th key={i} scope="col">
                    {data.headers[i] ?? ''}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {data.rows.map((row, r) => (
                <tr key={offset + r}>
                  <td className="num text-muted">{offset + r + 1}</td>
                  {Array.from({ length: columns }, (_, i) => (
                    <td key={i} className="max-w-sm truncate whitespace-nowrap" title={row[i] ?? ''}>
                      {row[i] ?? ''}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </TableFrame>

      <div className="shrink-0">
        <Pager
          offset={offset}
          limit={PAGE_SIZE}
          total={data?.total ?? 0}
          busy={q.isFetching}
          onOffset={(o) => setPosition({ scope, offset: o })}
        />
      </div>
    </div>
  );
}
