import { useEffect, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api, type FilePreview, type PreviewEntry } from '../api';
import { formatCount } from '../lib/format';
import { baseName } from '../lib/path';
import { qk } from '../lib/queries';
import { useApp } from '../store';
import { PathButtons } from './PathButtons';
import { TableViewer } from './TableViewer';
import { Button, DateText, ErrorText, Loading, Notice, Size, TableFrame } from './ui';

function ImageView({ path, name }: { path: string; name: string }) {
  const [actualSize, setActualSize] = useState(false);
  const [failed, setFailed] = useState(false);
  if (failed) return <Notice kind="warn">This picture could not be shown here. Use “Open in Windows” above.</Notice>;
  return (
    <div
      className={`min-h-0 flex-1 rounded-md border border-line bg-surface-3 ${actualSize ? 'overflow-auto' : 'overflow-hidden'}`}
    >
      <button
        type="button"
        onClick={() => setActualSize((v) => !v)}
        title={actualSize ? 'Click to fit the picture to the panel' : 'Click to see it at 100%'}
        className={actualSize ? 'block cursor-zoom-out' : 'block h-full w-full cursor-zoom-in'}
      >
        <img
          src={api.fileUrl(path)}
          alt={name}
          onError={() => setFailed(true)}
          className={actualSize ? 'max-w-none' : 'h-full w-full object-contain'}
        />
      </button>
    </div>
  );
}

function TextView({ data }: { data: FilePreview }) {
  return (
    <>
      {data.truncated && <Notice>Showing the first 256 KB</Notice>}
      <div
        role="region"
        aria-label="File text"
        tabIndex={0}
        className="min-h-0 flex-1 overflow-auto rounded-md border border-line bg-surface-3 p-3"
      >
        <pre className="whitespace-pre-wrap break-words font-mono text-xs leading-relaxed">{data.text ?? ''}</pre>
      </div>
    </>
  );
}

function FolderView({ data, onOpen }: { data: FilePreview; onOpen: (entry: PreviewEntry) => void }) {
  const entries = data.entries ?? [];
  return (
    <>
      {data.entries_total > entries.length && (
        <Notice>
          Showing {formatCount(entries.length)} of {formatCount(data.entries_total)}
        </Notice>
      )}
      <TableFrame label="Folder contents">
        {entries.length === 0 ? (
          <p className="p-4 text-sm font-medium">This folder is empty.</p>
        ) : (
          <table className="tbl">
            <thead>
              <tr>
                <th>Name</th>
                <th className="num">Size</th>
                <th>Modified</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((e) => (
                <tr key={e.path} className="clickable" onClick={() => onOpen(e)}>
                  <td className="break-all">
                    <button type="button" className="text-left font-medium hover:underline" title={e.path}>
                      {e.is_dir ? 'Folder · ' : ''}
                      {e.name}
                    </button>
                  </td>
                  <td className="num">{e.is_dir ? <span className="text-muted">—</span> : <Size bytes={e.size} />}</td>
                  <td>
                    <DateText ms={e.modified_ms} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </TableFrame>
    </>
  );
}

function PreviewBody({
  data,
  path,
  asTable,
  onOpen,
}: {
  data: FilePreview;
  path: string;
  asTable: boolean;
  onOpen: (entry: PreviewEntry) => void;
}) {
  if (!data.exists) {
    return (
      <Notice kind="warn">
        <p className="font-bold">This file is not there any more.</p>
        <p className="mt-1 break-all font-normal">{path}</p>
        {data.note && <p className="mt-1 font-normal">{data.note}</p>}
      </Notice>
    );
  }
  if (asTable) return <TableViewer path={path} />;
  const url = api.fileUrl(path);
  switch (data.kind) {
    case 'image':
      return <ImageView path={path} name={data.name} />;
    case 'pdf':
      return (
        <iframe
          title={data.name}
          src={url}
          className="min-h-0 w-full flex-1 rounded-md border border-line bg-white"
        />
      );
    case 'video':
      return (
        <video controls preload="metadata" src={url} className="min-h-0 w-full flex-1 rounded-md bg-black">
          <track kind="captions" />
        </video>
      );
    case 'audio':
      return <audio controls preload="metadata" src={url} className="w-full" />;
    case 'text':
      return <TextView data={data} />;
    case 'folder':
      return <FolderView data={data} onOpen={onOpen} />;
    case 'table':
      return <TableViewer path={path} />;
    case 'none':
      return (
        <Notice>
          {data.note ?? 'There is no preview for this kind of file. Use “Open in Windows” above to see it.'}
        </Notice>
      );
  }
}

function Drawer({ root, forceTable }: { root: string; forceTable: boolean }) {
  const close = useApp((s) => s.closePreview);
  const [trail, setTrail] = useState<string[]>([root]);
  const path = trail[trail.length - 1];
  const panelRef = useRef<HTMLElement>(null);

  const q = useQuery({
    queryKey: [...qk.preview, path],
    queryFn: () => api.previewFile(path),
    staleTime: 0,
  });
  const data = q.data;

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    panelRef.current?.focus();
    // Capture phase: Esc closes the drawer first and never reaches a dialog underneath it.
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== 'Escape') return;
      e.stopImmediatePropagation();
      close();
    };
    window.addEventListener('keydown', onKey, true);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      opener?.focus();
    };
  }, [close]);

  const open = (entry: PreviewEntry): void => setTrail((t) => [...t, entry.path]);
  const name = data?.name || baseName(path);

  return (
    <aside
      ref={panelRef}
      role="dialog"
      aria-label={`Preview of ${name}`}
      tabIndex={-1}
      className="fixed inset-y-0 right-0 z-[60] flex w-full flex-col border-l-4 border-accent bg-surface shadow-2xl outline-none lg:w-[45%] lg:min-w-[30rem]"
    >
      <div className="flex shrink-0 flex-col gap-1.5 border-b border-line px-4 py-3">
        <div className="flex items-start justify-between gap-3">
          <h2 className="break-all text-lg font-bold leading-snug">{name}</h2>
          <Button small onClick={close} aria-label="Close the preview">
            Close
          </Button>
        </div>
        {trail.length > 1 && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <Button small onClick={() => setTrail((t) => t.slice(0, -1))}>
              Back
            </Button>
            <span className="text-muted" aria-label="Where you are">
              {trail.map(baseName).join(' › ')}
            </span>
          </div>
        )}
        <p className="break-all text-xs text-muted">{path}</p>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-sm">
          {data?.exists && !data.is_dir && (
            <span>
              <Size bytes={data.size} />
            </span>
          )}
          {data?.exists && data.modified_ms > 0 && (
            <span>
              Modified <DateText ms={data.modified_ms} />
            </span>
          )}
          <PathButtons path={path} openLabel="Open in Windows" />
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-hidden p-3">
        {q.isPending && <Loading text="Opening…" />}
        <ErrorText error={q.error} />
        {data && <PreviewBody data={data} path={path} asTable={forceTable && trail.length === 1} onOpen={open} />}
      </div>
    </aside>
  );
}

/** The one right-hand preview panel. Lives at the top of the app, over whichever tab is open. */
export function PreviewDrawer() {
  const preview = useApp((s) => s.preview);
  if (!preview) return null;
  return <Drawer key={preview.nonce} root={preview.path} forceTable={preview.table} />;
}
