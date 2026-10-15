import { useMutation } from '@tanstack/react-query';
import { api, inTauri } from './api';
import { Button, ErrorText } from './components/ui';
import { isEngineMissing } from './lib/errors';
import { ENGINE_MISSING_TEXT } from './lib/labels';
import { useAppInfo } from './lib/queries';
import { useEngineSync } from './lib/useEngineSync';
import { ActionsTab } from './tabs/ActionsTab';
import { DrivesTab } from './tabs/DrivesTab';
import { DuplicatesTab } from './tabs/DuplicatesTab';
import { FilesTab } from './tabs/FilesTab';
import { ProgressTab } from './tabs/ProgressTab';
import { ReportsTab } from './tabs/ReportsTab';
import { SettingsTab } from './tabs/SettingsTab';
import { SummaryTab } from './tabs/SummaryTab';
import { isScanActive, useApp, type TabId } from './store';

const TABS: { id: TabId; label: string }[] = [
  { id: 'drives', label: 'Drives' },
  { id: 'progress', label: 'Progress' },
  { id: 'summary', label: 'Summary' },
  { id: 'files', label: 'Files' },
  { id: 'duplicates', label: 'Duplicates' },
  { id: 'actions', label: 'Plan & Actions' },
  { id: 'reports', label: 'Reports' },
  { id: 'settings', label: 'Settings' },
];

function ActivePanel({ tab }: { tab: TabId }) {
  switch (tab) {
    case 'drives':
      return <DrivesTab />;
    case 'progress':
      return <ProgressTab />;
    case 'summary':
      return <SummaryTab />;
    case 'files':
      return <FilesTab />;
    case 'duplicates':
      return <DuplicatesTab />;
    case 'actions':
      return <ActionsTab />;
    case 'reports':
      return <ReportsTab />;
    case 'settings':
      return <SettingsTab />;
  }
}

function ThemeToggle() {
  const theme = useApp((s) => s.theme);
  const toggle = useApp((s) => s.toggleTheme);
  const next = theme === 'dark' ? 'light' : 'dark';
  return (
    <button
      type="button"
      onClick={toggle}
      className="rounded-md border border-white/40 px-3 py-1 text-sm font-medium text-white hover:bg-white/10"
      title={`Switch to ${next} mode`}
    >
      {theme === 'dark' ? 'Light mode' : 'Dark mode'}
    </button>
  );
}

function Header({ connected }: { connected: boolean }) {
  const infoQ = useAppInfo(connected);
  const info = infoQ.data;
  return (
    <header className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b-4 border-accent bg-header px-4 py-2.5 text-white">
      <h1 className="text-lg font-bold tracking-tight">AVNI Drive Scanner</h1>
      {info && <span className="text-sm text-white/80">v{info.version}</span>}
      {info && (
        <span
          className={`rounded-full border px-2.5 py-0.5 text-xs font-bold ${
            info.elevated ? 'border-accent bg-accent text-[#111418]' : 'border-white/50 text-white'
          }`}
          title={info.elevated ? 'Running with administrator rights' : 'Running without administrator rights'}
        >
          {info.elevated ? 'Administrator' : 'Standard user'}
        </span>
      )}
      <div className="ml-auto">
        <ThemeToggle />
      </div>
    </header>
  );
}

function ElevationStrip() {
  const restart = useMutation({ mutationFn: () => api.restartElevated() });
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-yellow-700 bg-yellow-300 px-4 py-2 text-sm font-medium text-[#1a1400]">
      <span>Fast mode (reads the NTFS master file table — seconds instead of minutes) needs administrator rights.</span>
      <Button
        small
        variant="onYellow"
        busy={restart.isPending}
        busyLabel="Waiting for Windows…"
        onClick={() => restart.mutate()}
      >
        Restart as administrator
      </Button>
      <ErrorText error={restart.error} className="basis-full" />
    </div>
  );
}

function TabBar() {
  const tab = useApp((s) => s.tab);
  const setTab = useApp((s) => s.setTab);
  const scanning = useApp((s) => isScanActive(s.scan));
  const acting = useApp((s) => s.actionRunning);
  const busyTabs: Partial<Record<TabId, boolean>> = { progress: scanning, actions: acting };
  return (
    <nav aria-label="Sections" className="flex shrink-0 overflow-x-auto border-b border-line bg-surface">
      {TABS.map((t) => {
        const active = t.id === tab;
        return (
          <button
            key={t.id}
            type="button"
            aria-current={active ? 'page' : undefined}
            onClick={() => setTab(t.id)}
            className={`flex shrink-0 items-center gap-2 whitespace-nowrap border-b-4 px-4 py-2.5 text-sm ${
              active
                ? 'border-accent font-bold text-fg'
                : 'border-transparent font-medium text-muted hover:bg-surface-3 hover:text-fg'
            }`}
          >
            {t.label}
            {busyTabs[t.id] && (
              <span className="size-2.5 animate-pulse rounded-full bg-accent" title="Working…" aria-label="working" />
            )}
          </button>
        );
      })}
    </nav>
  );
}

/** Shown instead of everything else when there is no engine. No tables, no sample data. */
function EngineMissingNotice() {
  return (
    <main className="flex min-h-0 flex-1 p-4">
      <div
        role="alert"
        className="flex w-full items-center justify-center rounded-lg border-2 border-warn bg-warn-bg p-8 text-center text-xl font-bold text-warn"
      >
        {ENGINE_MISSING_TEXT}
      </div>
    </main>
  );
}

export function App() {
  const connected = inTauri();
  useEngineSync(connected);
  const tab = useApp((s) => s.tab);
  const infoQ = useAppInfo(connected);
  const missing = !connected || (infoQ.isError && isEngineMissing(infoQ.error));

  return (
    <div className="flex h-dvh flex-col bg-bg text-fg">
      <Header connected={connected} />
      {missing ? (
        <EngineMissingNotice />
      ) : (
        <>
          {infoQ.data && !infoQ.data.elevated && <ElevationStrip />}
          <TabBar />
          <main className="min-h-0 flex-1">
            <ActivePanel tab={tab} />
          </main>
        </>
      )}
    </div>
  );
}
