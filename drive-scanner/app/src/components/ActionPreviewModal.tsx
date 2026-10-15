import { useMemo, useState } from 'react';
import { useMutation, useQuery } from '@tanstack/react-query';
import { ACTION_LABEL, api, type ActionKind, type ActionRequest } from '../api';
import { formatCount } from '../lib/format';
import { useApp } from '../store';
import { Button, CheckField, ErrorText, Loading, Modal, Notice, Size, TableFrame, VerdictPill } from './ui';

const SAMPLE_ROWS = 200;

export function ActionPreviewModal({
  kind,
  drives,
  onClose,
}: {
  kind: ActionKind;
  drives: string[];
  onClose: () => void;
}) {
  const req = useMemo<ActionRequest>(() => ({ kind, ids: null, drives }), [kind, drives]);
  const preview = useQuery({
    queryKey: ['preview', kind, drives],
    queryFn: () => api.previewAction(req),
    gcTime: 0,
    staleTime: 0,
  });
  const [understood, setUnderstood] = useState(false);

  const run = useMutation({
    mutationFn: () => api.runAction(req),
    onSuccess: () => {
      useApp.getState().beginAction();
      onClose();
    },
  });

  const data = preview.data;
  const blocked = (data?.blocked.length ?? 0) > 0;
  const needsTick = kind === 'empty_recycle_bin';
  const canRun = !!data && !blocked && (!needsTick || understood);
  const rows = data?.sample.slice(0, SAMPLE_ROWS) ?? [];

  return (
    <Modal
      title={ACTION_LABEL[kind]}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Close</Button>
          <Button
            variant={kind === 'extract' || kind === 'publish_supabase' ? 'primary' : 'danger'}
            busy={run.isPending}
            busyLabel="Starting…"
            disabled={!canRun}
            onClick={() => run.mutate()}
          >
            Run
          </Button>
        </>
      }
    >
      <p className="text-sm">
        <strong>Drives:</strong> {drives.length === 0 ? 'all scanned drives' : drives.join(', ')}
      </p>
      <p className="text-sm text-muted">This is only a preview. Nothing is changed until you press Run.</p>

      {preview.isPending && <Loading text="Checking what would happen…" />}
      <ErrorText error={preview.error} />

      {data && (
        <>
          <dl className="grid grid-cols-2 gap-3 rounded-md border border-line bg-surface-3 p-3 sm:grid-cols-3">
            <div>
              <dt className="text-xs font-semibold uppercase tracking-wide text-muted">Files</dt>
              <dd className="text-lg font-bold tabular-nums">{formatCount(data.files)}</dd>
            </div>
            <div>
              <dt className="text-xs font-semibold uppercase tracking-wide text-muted">Size</dt>
              <dd className="text-lg font-bold">
                <Size bytes={data.bytes} />
              </dd>
            </div>
            <div className="col-span-2 sm:col-span-1">
              <dt className="text-xs font-semibold uppercase tracking-wide text-muted">Goes to</dt>
              <dd className="break-all text-sm font-semibold">{data.target ?? '—'}</dd>
            </div>
          </dl>

          {data.warnings.length > 0 && (
            <Notice kind="warn">
              <ul className="list-disc space-y-0.5 pl-5">
                {data.warnings.map((w, i) => (
                  <li key={`${i}-${w}`}>{w}</li>
                ))}
              </ul>
            </Notice>
          )}
          {blocked && (
            <Notice kind="danger">
              <p className="mb-1 font-bold">This cannot run yet:</p>
              <ul className="list-disc space-y-0.5 pl-5">
                {data.blocked.map((b, i) => (
                  <li key={`${i}-${b}`}>{b}</li>
                ))}
              </ul>
            </Notice>
          )}
          {data.files === 0 && !blocked && <Notice>Nothing matches, so running this would do nothing.</Notice>}

          {rows.length > 0 && (
            <>
              <p className="text-sm font-semibold">
                First {formatCount(rows.length)} of {formatCount(data.files)} files
              </p>
              <TableFrame label="Sample of the files" fit className="max-h-64">
                <table className="tbl">
                  <thead>
                    <tr>
                      <th>Path</th>
                      <th className="num">Size</th>
                      <th>Verdict</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((r) => (
                      <tr key={r.id}>
                        <td className="w-full min-w-56 break-all" title={r.path}>
                          {r.path}
                        </td>
                        <td className="num">
                          <Size bytes={r.size} />
                        </td>
                        <td>
                          <VerdictPill verdict={r.verdict} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </TableFrame>
            </>
          )}

          {needsTick && !blocked && (
            <CheckField
              label="I understand this cannot be undone."
              checked={understood}
              onChange={setUnderstood}
            />
          )}
        </>
      )}
      <ErrorText error={run.error} />
    </Modal>
  );
}
