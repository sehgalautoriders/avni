import { useState } from 'react';
import { keepPreviousData, useQuery } from '@tanstack/react-query';
import { api, type DupGroup } from '../api';
import {
  CheckField,
  DateText,
  ErrorText,
  Loading,
  NoScanYet,
  Pager,
  Panel,
  Size,
  TableFrame,
} from '../components/ui';
import { formatCount } from '../lib/format';
import { qk, useSummary } from '../lib/queries';

const GROUPS_PER_PAGE = 50;

function GroupRows({ g }: { g: DupGroup }) {
  return (
    <>
      <tr className="grp">
        <td colSpan={3}>
          <span className="flex flex-wrap items-center gap-x-4 gap-y-1">
            <span>Group #{g.id}</span>
            <span>
              <Size bytes={g.size} /> each
            </span>
            <span>{formatCount(g.members.length)} copies</span>
            <span>
              reclaimable <Size bytes={g.wasted} />
            </span>
            {g.confirmed ? (
              <span className="text-ok">Confirmed ✓</span>
            ) : (
              <span className="font-normal text-muted">Sampled — verified before removal</span>
            )}
          </span>
        </td>
      </tr>
      {g.members.map((m, i) => {
        // THE TREE LAW: the first copy is kept; only copies it marked loose carry the Duplicate verdict.
        const role = i === 0 ? 'keep' : m.verdict === 'duplicate' ? 'remove' : 'law';
        return (
        <tr key={m.id}>
          <td className="w-36">
            <span
              className={`inline-block rounded-full border px-2 py-0.5 text-xs font-semibold ${
                role === 'remove'
                  ? 'border-danger bg-danger-bg text-danger'
                  : 'border-ok bg-ok-bg text-ok'
              }`}
              title={m.reason}
            >
              {role === 'keep' ? 'Keep' : role === 'remove' ? 'Remove' : 'Kept (tree law)'}
            </span>
          </td>
          <td className="break-all" title={m.reason}>
            {m.path}
            <div className="text-xs text-muted">{m.reason}</div>
          </td>
          <td className="w-40">
            <DateText ms={m.modified_ms} />
          </td>
        </tr>
        );
      })}
    </>
  );
}

export function DuplicatesTab() {
  const summaryQ = useSummary();
  const [confirmedOnly, setConfirmedOnly] = useState(false);
  const [offset, setOffset] = useState(0);

  const dupsQ = useQuery({
    queryKey: [...qk.dups, offset, GROUPS_PER_PAGE, confirmedOnly],
    queryFn: () => api.queryDuplicates(offset, GROUPS_PER_PAGE, confirmedOnly),
    placeholderData: keepPreviousData,
    enabled: !!summaryQ.data,
  });

  if (summaryQ.isPending) {
    return (
      <Panel label="Duplicates">
        <Loading />
      </Panel>
    );
  }
  if (!summaryQ.data) {
    return (
      <Panel label="Duplicates">
        <ErrorText error={summaryQ.error} />
        <NoScanYet />
      </Panel>
    );
  }

  const page = dupsQ.data;
  return (
    <Panel label="Duplicates">
      <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
        <CheckField
          label="Only confirmed (full hash)"
          checked={confirmedOnly}
          onChange={(v) => {
            setConfirmedOnly(v);
            setOffset(0);
          }}
        />
        <p className="text-sm text-muted">
          THE TREE LAW decides each group: the first copy is kept; only loose copies (marked Remove) go to the Recycle Bin
          when you run “Remove duplicates” in Plan &amp; Actions. Filed copies are kept.
        </p>
      </div>
      <ErrorText error={dupsQ.error} />

      <TableFrame label="Duplicate groups" className={dupsQ.isFetching && page ? 'opacity-70' : ''}>
        {!page ? (
          <div className="p-4">
            <Loading text="Loading duplicates…" />
          </div>
        ) : page.groups.length === 0 ? (
          <p className="p-4 text-sm font-medium">
            {confirmedOnly ? 'No confirmed duplicates.' : 'No duplicates were found.'}
          </p>
        ) : (
          <table className="tbl">
            <thead>
              <tr>
                <th>Keep or remove</th>
                <th>Path</th>
                <th>Modified</th>
              </tr>
            </thead>
            <tbody>
              {page.groups.map((g) => (
                <GroupRows key={g.id} g={g} />
              ))}
            </tbody>
          </table>
        )}
      </TableFrame>

      <div className="flex shrink-0 items-center gap-3">
        <Pager
          offset={offset}
          limit={GROUPS_PER_PAGE}
          total={page?.total ?? 0}
          noun="groups"
          busy={dupsQ.isFetching}
          onOffset={setOffset}
        />
      </div>
    </Panel>
  );
}
