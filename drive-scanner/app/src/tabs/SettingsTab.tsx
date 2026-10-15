import { useState } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { api, type AppInfo, type Settings } from '../api';
import { Button, CheckField, ErrorText, Field, INPUT_CLASS, Loading, Notice, Panel, SectionTitle } from '../components/ui';
import { trimNumber } from '../lib/format';
import { qk, useAppInfo, useSettings } from '../lib/queries';

/** The form keeps everything as text so half-typed numbers are fine; it is checked on Save. */
interface FormState {
  protectedPaths: string;
  gdriveRoot: string;
  transferFolder: string;
  markers: string;
  sevenzip: string;
  reportsDir: string;
  supabaseUrl: string;
  supabaseTable: string;
  dedupKb: string;
  budgetMin: string;
  reserveGb: string;
  depth: string;
  ratio: string;
  home: string;
  homeClears: boolean;
  gdriveAccount: string;
}

const toForm = (s: Settings): FormState => ({
  protectedPaths: s.protected_paths.join('\n'),
  gdriveRoot: s.gdrive_root,
  transferFolder: s.transfer_folder,
  markers: s.processed_markers.join(', '),
  sevenzip: s.sevenzip_path,
  reportsDir: s.reports_dir,
  supabaseUrl: s.supabase_url,
  supabaseTable: s.supabase_table,
  dedupKb: trimNumber(s.dedup_min_size / 1024),
  budgetMin: trimNumber(s.time_budget_secs / 60),
  reserveGb: trimNumber(s.c_reserve_gb),
  depth: String(s.extract_max_depth),
  ratio: trimNumber(s.extract_max_ratio),
  home: s.home.join('\n'),
  homeClears: s.home_clears,
  gdriveAccount: s.gdrive_account,
});

function parseNumber(text: string, what: string, whole = false): number | string {
  const n = Number(text);
  if (text.trim() === '' || !Number.isFinite(n) || n < 0) return `${what} must be a number, 0 or more.`;
  if (whole && !Number.isInteger(n)) return `${what} must be a whole number.`;
  return n;
}

const splitLines = (text: string): string[] =>
  text
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l !== '');

const splitCommas = (text: string): string[] =>
  text
    .split(',')
    .map((l) => l.trim())
    .filter((l) => l !== '');

/** Turns the form back into Settings, or says what is wrong. */
function fromForm(f: FormState, base: Settings): Settings | string {
  const kb = parseNumber(f.dedupKb, 'Duplicate minimum size');
  if (typeof kb === 'string') return kb;
  const min = parseNumber(f.budgetMin, 'Time budget');
  if (typeof min === 'string') return min;
  const gb = parseNumber(f.reserveGb, 'C: reserve');
  if (typeof gb === 'string') return gb;
  const depth = parseNumber(f.depth, 'Extract depth', true);
  if (typeof depth === 'string') return depth;
  const ratio = parseNumber(f.ratio, 'Extract ratio');
  if (typeof ratio === 'string') return ratio;
  return {
    ...base,
    protected_paths: splitLines(f.protectedPaths),
    gdrive_root: f.gdriveRoot.trim(),
    transfer_folder: f.transferFolder.trim(),
    processed_markers: splitCommas(f.markers),
    sevenzip_path: f.sevenzip.trim(),
    reports_dir: f.reportsDir.trim(),
    supabase_url: f.supabaseUrl.trim(),
    supabase_table: f.supabaseTable.trim(),
    dedup_min_size: Math.round(kb * 1024),
    time_budget_secs: Math.round(min * 60),
    c_reserve_gb: gb,
    extract_max_depth: depth,
    extract_max_ratio: ratio,
    home: splitLines(f.home),
    home_clears: f.homeClears,
    gdrive_account: f.gdriveAccount.trim(),
  };
}

function TextInput({
  label,
  hint,
  value,
  onChange,
  placeholder,
  type = 'text',
}: {
  label: string;
  hint?: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  type?: 'text' | 'password' | 'number';
}) {
  return (
    <Field label={label} hint={hint}>
      {(id) => (
        <input
          id={id}
          type={type}
          min={type === 'number' ? 0 : undefined}
          step={type === 'number' ? 'any' : undefined}
          inputMode={type === 'number' ? 'decimal' : undefined}
          autoComplete={type === 'password' ? 'new-password' : 'off'}
          spellCheck={false}
          className={INPUT_CLASS}
          value={value}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
    </Field>
  );
}

function SettingsForm({ initial }: { initial: Settings }) {
  const qc = useQueryClient();
  const [form, setForm] = useState<FormState>(() => toForm(initial));
  const [key, setKey] = useState('');
  const [problem, setProblem] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const edit = (patch: Partial<FormState>): void => {
    setSaved(false);
    setProblem(null);
    setForm((f) => ({ ...f, ...patch }));
  };

  const save = useMutation({
    mutationFn: ({ settings, supabaseKey }: { settings: Settings; supabaseKey: string | null }) =>
      api.saveSettings(settings, supabaseKey),
    onSuccess: (fresh) => {
      qc.setQueryData(qk.settings, fresh);
      setForm(toForm(fresh));
      setKey('');
      setSaved(true);
    },
  });

  const removeKey = useMutation({
    // The stored settings are sent as they are, so half-typed edits are not saved by accident.
    mutationFn: () => api.saveSettings(initial, ''),
    onSuccess: (fresh) => {
      qc.setQueryData(qk.settings, fresh);
      setSaved(true);
    },
  });

  const onSubmit = (): void => {
    const settings = fromForm(form, initial);
    if (typeof settings === 'string') {
      setProblem(settings);
      return;
    }
    setProblem(null);
    save.mutate({ settings, supabaseKey: key === '' ? null : key });
  };

  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        onSubmit();
      }}
    >
      <div className="grid gap-4 md:grid-cols-2">
        <Field label="Protected paths" hint="One per line. The app never moves or deletes anything inside these." className="md:row-span-2">
          {(id) => (
            <textarea
              id={id}
              rows={6}
              spellCheck={false}
              className={`${INPUT_CLASS} h-full min-h-32 font-mono`}
              value={form.protectedPaths}
              onChange={(e) => edit({ protectedPaths: e.target.value })}
            />
          )}
        </Field>
        <TextInput
          label="Google Drive folder (for Transfer)"
          hint={'Required for Transfer: the "My Drive" folder of the AVNI upload account, e.g. G:\\My Drive. Never picked automatically.'}
          value={form.gdriveRoot}
          onChange={(v) => edit({ gdriveRoot: v })}
        />
        <TextInput
          label="Google account of that folder"
          hint="AVNI cloud account law: uploads go only to the AVNI upload account. The read-only account is never written to."
          placeholder="the AVNI upload account (…@gmail.com)"
          value={form.gdriveAccount}
          onChange={(v) => edit({ gdriveAccount: v })}
        />
        <TextInput
          label="Transfer folder name"
          hint="The folder inside Google Drive that receives transferred files."
          value={form.transferFolder}
          onChange={(v) => edit({ transferFolder: v })}
        />
        <TextInput
          label="Processed markers"
          hint="Comma list. Files whose path has one of these words count as already processed."
          value={form.markers}
          onChange={(v) => edit({ markers: v })}
        />
        <TextInput
          label="7-Zip path"
          hint="blank = find it automatically"
          value={form.sevenzip}
          onChange={(v) => edit({ sevenzip: v })}
        />
        <TextInput
          label="Reports folder"
          hint="blank = the default folder"
          value={form.reportsDir}
          onChange={(v) => edit({ reportsDir: v })}
        />
      </div>

      <fieldset className="grid gap-3 rounded-md border border-line p-3 md:grid-cols-2">
        <legend className="px-1 text-sm font-bold">Duplicates — THE TREE LAW</legend>
        <p className="text-sm text-muted md:col-span-2">
          A copy filed inside an organised tree is never the one removed. Only a loose copy may go: in a dumping ground
          (Downloads, Temp, New folder…), named like a copy (“x - Copy”, “x (2)”), pulled out of its own folder, beside the
          kept copy, or tipped into a flat heap. Programs, projects, system and protected folders are never touched.
        </p>
        <Field label="Home places (most wanted first)" hint="One per line: a drive (D:\) or a folder. Chooses which copy is KEPT. The tree law still wins every collision.">
          {(id) => (
            <textarea
              id={id}
              rows={3}
              spellCheck={false}
              className={`${INPUT_CLASS} font-mono`}
              value={form.home}
              onChange={(e) => edit({ home: e.target.value })}
            />
          )}
        </Field>
        <div className="flex flex-col gap-2">
          <CheckField
            label={<strong>HOME CLEARS</strong>}
            checked={form.homeClears}
            onChange={(v) => edit({ homeClears: v })}
          />
          <p className="text-sm text-muted">
            Off by default. When ticked, a FILED copy outside every home place may also go — only if the copy kept is
            itself filed inside a home place. Programs, projects, system and protected folders stay untouchable.
          </p>
        </div>
      </fieldset>

      <fieldset className="grid gap-4 rounded-md border border-line p-3 md:grid-cols-2">
        <legend className="px-1 text-sm font-bold">Supabase</legend>
        <TextInput label="Supabase URL" value={form.supabaseUrl} onChange={(v) => edit({ supabaseUrl: v })} />
        <TextInput label="Supabase table" value={form.supabaseTable} onChange={(v) => edit({ supabaseTable: v })} />
        <div className="md:col-span-2">
          <TextInput
            label="Supabase key"
            type="password"
            hint="Stored encrypted and never shown again."
            placeholder={initial.supabase_key_set ? 'stored ✓ — leave blank to keep' : 'paste the key here'}
            value={key}
            onChange={(v) => {
              setSaved(false);
              setKey(v);
            }}
          />
          {initial.supabase_key_set && (
            <div className="mt-2">
              <Button
                small
                variant="danger"
                busy={removeKey.isPending}
                busyLabel="Removing…"
                onClick={() => removeKey.mutate()}
              >
                Remove key
              </Button>
            </div>
          )}
        </div>
      </fieldset>

      <fieldset className="grid gap-4 rounded-md border border-line p-3 sm:grid-cols-2 lg:grid-cols-5">
        <legend className="px-1 text-sm font-bold">Limits</legend>
        <TextInput
          label="Duplicate min size (KB)"
          type="number"
          hint="Smaller files are not checked for duplicates."
          value={form.dedupKb}
          onChange={(v) => edit({ dedupKb: v })}
        />
        <TextInput
          label="Time budget (min)"
          type="number"
          value={form.budgetMin}
          onChange={(v) => edit({ budgetMin: v })}
        />
        <TextInput
          label="C: reserve (GB)"
          type="number"
          hint="Free space to keep on C:."
          value={form.reserveGb}
          onChange={(v) => edit({ reserveGb: v })}
        />
        <TextInput
          label="Extract max depth"
          type="number"
          hint="How many archives-inside-archives to open."
          value={form.depth}
          onChange={(v) => edit({ depth: v })}
        />
        <TextInput
          label="Extract max ratio"
          type="number"
          hint="Stops zip bombs."
          value={form.ratio}
          onChange={(v) => edit({ ratio: v })}
        />
      </fieldset>

      <div className="flex flex-wrap items-center gap-3">
        <Button type="submit" variant="primary" busy={save.isPending} busyLabel="Saving…">
          Save settings
        </Button>
        {saved && <span className="font-semibold text-ok">Saved ✓</span>}
      </div>
      {problem && <ErrorText error={problem} />}
      <ErrorText error={save.error ?? removeKey.error} />
    </form>
  );
}

function Found({ label, path }: { label: string; path: string | null }) {
  return (
    <tr>
      <th scope="row" className="pr-4 text-left font-semibold">
        {label}
      </th>
      <td className="break-all">
        {path ? <span title={path}>found at {path}</span> : <span className="font-semibold text-warn">not found</span>}
      </td>
    </tr>
  );
}

function AboutThisApp({ info }: { info: AppInfo }) {
  return (
    <section aria-label="About this computer" className="rounded-md border border-line bg-surface p-3">
      <SectionTitle>About this computer</SectionTitle>
      <table className="mt-2 w-full text-sm">
        <tbody className="[&_td]:py-1 [&_th]:py-1 [&_th]:align-top [&_td]:align-top">
          <tr>
            <th scope="row" className="pr-4 text-left font-semibold">
              Computer name
            </th>
            <td>{info.machine}</td>
          </tr>
          <tr>
            <th scope="row" className="pr-4 text-left font-semibold">
              Settings file
            </th>
            <td className="break-all">{info.settings_path}</td>
          </tr>
          <tr>
            <th scope="row" className="pr-4 text-left font-semibold">
              Reports folder
            </th>
            <td className="break-all">{info.reports_dir}</td>
          </tr>
          <Found label="7-Zip" path={info.sevenzip} />
          <Found label="Google Drive" path={info.gdrive_root} />
        </tbody>
      </table>
    </section>
  );
}

export function SettingsTab() {
  const settingsQ = useSettings();
  const infoQ = useAppInfo();
  return (
    <Panel label="Settings">
      <ErrorText error={settingsQ.error ?? infoQ.error} />
      {settingsQ.isPending && <Loading text="Loading settings…" />}
      {settingsQ.data && <SettingsForm initial={settingsQ.data} />}
      {infoQ.data && <AboutThisApp info={infoQ.data} />}
      {settingsQ.isSuccess && (
        <Notice>Changes only take effect after you press Save settings.</Notice>
      )}
    </Panel>
  );
}
