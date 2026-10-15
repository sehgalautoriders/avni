import {
  useEffect,
  useId,
  useRef,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type KeyboardEventHandler,
  type ReactNode,
} from 'react';
import { VERDICT_LABEL, type SortKey, type Verdict } from '../api';
import { errMsg } from '../lib/errors';
import { exactBytes, formatBytes, formatCount, formatDate } from '../lib/format';

/* ---------- Buttons ---------- */

type Variant = 'primary' | 'secondary' | 'danger' | 'ghost' | 'onYellow';

const VARIANT_CLASS: Record<Variant, string> = {
  primary: 'border-transparent bg-accent font-semibold text-[#111418] hover:brightness-110',
  secondary: 'border-line bg-surface text-fg hover:bg-surface-3',
  danger: 'border-transparent bg-red-700 font-semibold text-white hover:bg-red-600',
  ghost: 'border-transparent bg-transparent text-fg hover:bg-surface-3',
  /** Dark button for the always-yellow warning strip (same look in light and dark mode). */
  onYellow: 'border-transparent bg-[#1a1400] font-semibold text-white hover:bg-black',
};

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  busy?: boolean;
  busyLabel?: string;
  variant?: Variant;
  small?: boolean;
}

export function Spinner() {
  return (
    <span
      aria-hidden="true"
      className="inline-block size-4 shrink-0 animate-spin rounded-full border-2 border-current border-t-transparent"
    />
  );
}

/** Every button shows a spinner and is disabled while its work is running. */
export function Button({
  busy = false,
  busyLabel,
  variant = 'secondary',
  small = false,
  className = '',
  disabled,
  children,
  type = 'button',
  ...rest
}: ButtonProps) {
  const size = small ? 'px-2.5 py-1 text-xs' : 'px-3.5 py-1.5 text-sm';
  return (
    <button
      {...rest}
      type={type}
      disabled={disabled || busy}
      aria-busy={busy}
      className={`inline-flex items-center justify-center gap-2 rounded-md border ${size} ${VARIANT_CLASS[variant]} disabled:cursor-not-allowed disabled:opacity-55 ${className}`}
    >
      {busy && <Spinner />}
      {busy && busyLabel ? busyLabel : children}
    </button>
  );
}

/* ---------- Messages ---------- */

/** Red inline error with the real message. Renders nothing when there is no error. */
export function ErrorText({ error, className = '' }: { error: unknown; className?: string }) {
  if (!error) return null;
  return (
    <p
      role="alert"
      className={`rounded-md border border-danger bg-danger-bg px-3 py-2 text-sm font-medium text-danger ${className}`}
    >
      {errMsg(error)}
    </p>
  );
}

type NoticeKind = 'info' | 'warn' | 'danger' | 'ok';

const NOTICE_CLASS: Record<NoticeKind, string> = {
  info: 'border-line bg-surface-3 text-fg',
  warn: 'border-warn bg-warn-bg text-warn',
  danger: 'border-danger bg-danger-bg text-danger',
  ok: 'border-ok bg-ok-bg text-ok',
};

export function Notice({ kind = 'info', children }: { kind?: NoticeKind; children: ReactNode }) {
  return (
    <div
      role={kind === 'danger' ? 'alert' : 'status'}
      className={`rounded-md border px-3 py-2 text-sm font-medium ${NOTICE_CLASS[kind]}`}
    >
      {children}
    </div>
  );
}

export function Loading({ text = 'Loading…' }: { text?: string }) {
  return (
    <p className="flex items-center gap-2 text-sm text-muted" role="status">
      <Spinner /> {text}
    </p>
  );
}

export function NoScanYet() {
  return (
    <div className="rounded-md border border-line bg-surface p-6 text-center text-base font-medium">
      No scan yet — go to Drives and press Start scan
    </div>
  );
}

/* ---------- Layout ---------- */

/** One tab's body. Fills the window; its tables scroll inside it. */
export function Panel({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section
      aria-label={label}
      className="flex h-full min-h-0 flex-col gap-3 overflow-x-hidden overflow-y-auto p-3"
    >
      {children}
    </section>
  );
}

/** The scrolling box around a table. Focusable so the keyboard can scroll it. */
export function TableFrame({
  label,
  children,
  className = '',
  fit = false,
  onKeyDown,
}: {
  label: string;
  children: ReactNode;
  className?: string;
  /** false (default): fill the space left in the panel. true: only as tall as the table, then scroll at `className`'s max height. */
  fit?: boolean;
  onKeyDown?: KeyboardEventHandler<HTMLDivElement>;
}) {
  const size = fit ? 'shrink-0' : 'min-h-40 flex-1';
  return (
    <div
      role="region"
      aria-label={label}
      tabIndex={0}
      onKeyDown={onKeyDown}
      className={`${size} overflow-auto rounded-md border border-line bg-surface ${className}`}
    >
      {children}
    </div>
  );
}

export function SectionTitle({ children }: { children: ReactNode }) {
  return <h2 className="text-base font-bold">{children}</h2>;
}

/* ---------- Form bits ---------- */

export const INPUT_CLASS =
  'rounded-md border border-line bg-surface px-2.5 py-1.5 text-sm text-fg placeholder:text-muted disabled:opacity-60';

export function CheckField({
  label,
  checked,
  onChange,
  disabled,
}: {
  label: ReactNode;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <label className={`inline-flex items-center gap-2 text-sm ${disabled ? 'opacity-60' : ''}`}>
      <input
        type="checkbox"
        className="size-4 accent-accent"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span>{label}</span>
    </label>
  );
}

interface TickBoxProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'type'> {
  indeterminate?: boolean;
}

/** A checkbox that can show the "some are ticked" state. */
export function TickBox({ indeterminate = false, className = '', ...rest }: TickBoxProps) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate;
  }, [indeterminate]);
  return <input {...rest} ref={ref} type="checkbox" className={`size-4 accent-accent ${className}`} />;
}

export function Field({
  label,
  hint,
  children,
  className = '',
}: {
  label: string;
  hint?: string;
  children: (id: string) => ReactNode;
  className?: string;
}) {
  const id = useId();
  return (
    <div className={`flex flex-col gap-1 ${className}`}>
      <label htmlFor={id} className="text-sm font-semibold">
        {label}
      </label>
      {children(id)}
      {hint && <p className="text-xs text-muted">{hint}</p>}
    </div>
  );
}

/* ---------- Values ---------- */

const VERDICT_CLASS: Record<Verdict, string> = {
  keep: 'border-green-700/50 bg-green-100 text-green-900 dark:border-green-400/50 dark:bg-green-950 dark:text-green-200',
  protected: 'border-blue-700/50 bg-blue-100 text-blue-900 dark:border-blue-400/50 dark:bg-blue-950 dark:text-blue-200',
  transfer:
    'border-violet-700/50 bg-violet-100 text-violet-900 dark:border-violet-400/50 dark:bg-violet-950 dark:text-violet-200',
  in_drive: 'border-teal-700/50 bg-teal-100 text-teal-900 dark:border-teal-400/50 dark:bg-teal-950 dark:text-teal-200',
  duplicate:
    'border-amber-700/50 bg-amber-100 text-amber-900 dark:border-amber-400/50 dark:bg-amber-950 dark:text-amber-200',
  junk: 'border-red-700/50 bg-red-100 text-red-900 dark:border-red-400/50 dark:bg-red-950 dark:text-red-200',
  extract: 'border-pink-700/50 bg-pink-100 text-pink-900 dark:border-pink-400/50 dark:bg-pink-950 dark:text-pink-200',
  supabase: 'border-lime-700/50 bg-lime-100 text-lime-900 dark:border-lime-400/50 dark:bg-lime-950 dark:text-lime-200',
  review: 'border-slate-600/60 bg-slate-200 text-slate-900 dark:border-slate-400/60 dark:bg-slate-800 dark:text-slate-100',
};

export function VerdictPill({ verdict }: { verdict: Verdict }) {
  return (
    <span
      className={`inline-block whitespace-nowrap rounded-full border px-2 py-0.5 text-xs font-semibold ${VERDICT_CLASS[verdict]}`}
    >
      {VERDICT_LABEL[verdict]}
    </span>
  );
}

/** Human size with the exact byte count in the tooltip. */
export function Size({ bytes }: { bytes: number }) {
  return (
    <span title={exactBytes(bytes)} className="tabular-nums">
      {formatBytes(bytes)}
    </span>
  );
}

export function DateText({ ms }: { ms: number }) {
  return <span className="whitespace-nowrap tabular-nums">{formatDate(ms)}</span>;
}

/** The text that sits on a table's footer line, e.g. "rows 1–200 of 15,402". */
export function rangeText(offset: number, limit: number, total: number, noun: string): string {
  if (total <= 0) return `No ${noun}`;
  const from = offset + 1;
  const to = Math.min(offset + limit, total);
  return `${noun} ${formatCount(from)}–${formatCount(to)} of ${formatCount(total)}`;
}

export function Pager({
  offset,
  limit,
  total,
  noun = 'rows',
  busy = false,
  onOffset,
}: {
  offset: number;
  limit: number;
  total: number;
  noun?: string;
  busy?: boolean;
  onOffset: (offset: number) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <Button small disabled={offset <= 0 || busy} onClick={() => onOffset(Math.max(0, offset - limit))}>
        Prev
      </Button>
      <span className="min-w-44 text-center text-sm tabular-nums" aria-live="polite">
        {rangeText(offset, limit, total, noun)}
      </span>
      <Button small disabled={offset + limit >= total || busy} onClick={() => onOffset(offset + limit)}>
        Next
      </Button>
      {busy && <Spinner />}
    </div>
  );
}

export function SortTh({
  label,
  field,
  sort,
  desc,
  onSort,
  numeric = false,
}: {
  label: string;
  field: SortKey;
  sort: SortKey;
  desc: boolean;
  onSort: (field: SortKey) => void;
  numeric?: boolean;
}) {
  const active = sort === field;
  return (
    <th className={numeric ? 'num' : undefined} aria-sort={active ? (desc ? 'descending' : 'ascending') : 'none'}>
      <button
        type="button"
        onClick={() => onSort(field)}
        className="inline-flex items-center gap-1 rounded font-semibold hover:text-accent-text"
        title={`Sort by ${label.toLowerCase()}`}
      >
        {label}
        <span aria-hidden="true" className="inline-block w-3 text-xs">
          {active ? (desc ? '▼' : '▲') : ''}
        </span>
      </button>
    </th>
  );
}

/* ---------- Dialog ---------- */

export function Modal({
  title,
  onClose,
  children,
  footer,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer: ReactNode;
}) {
  const titleId = useId();
  const dialogRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  useEffect(() => {
    closeRef.current = onClose;
  });

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogRef.current?.focus();
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') closeRef.current();
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      opener?.focus();
    };
  }, []);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/65 p-3">
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        className="flex max-h-full w-full max-w-4xl flex-col rounded-lg border border-line bg-surface shadow-2xl outline-none"
      >
        <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-3">
          <h2 id={titleId} className="text-lg font-bold">
            {title}
          </h2>
          <Button small variant="ghost" onClick={onClose} aria-label="Close">
            Close
          </Button>
        </div>
        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">{children}</div>
        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-line px-4 py-3">{footer}</div>
      </div>
    </div>
  );
}
