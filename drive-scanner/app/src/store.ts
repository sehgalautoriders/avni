import { create } from 'zustand';
import type { ActionProgress, ActionResult, Phase, ReportFiles, ScanProgress } from './api';
import { applyTheme, readSavedTheme, saveTheme, systemTheme, type Theme } from './lib/theme';

export type TabId = 'drives' | 'progress' | 'summary' | 'files' | 'duplicates' | 'actions' | 'reports' | 'settings';

const ACTIVE_PHASES: Phase[] = ['metadata', 'rules', 'sampling', 'verifying', 'hashing'];
const FINISHED_PHASES: Phase[] = ['done', 'cancelled', 'failed'];

export const isScanActive = (p: ScanProgress | null): boolean => p !== null && ACTIVE_PHASES.includes(p.phase);
export const isScanFinished = (phase: Phase | undefined): boolean =>
  phase !== undefined && FINISHED_PHASES.includes(phase);

const MAX_REPORT_LINES = 200;

interface AppState {
  tab: TabId;
  setTab: (tab: TabId) => void;

  theme: Theme;
  savedChoice: boolean;
  toggleTheme: () => void;
  followSystem: (theme: Theme) => void;

  selectedDrives: string[];
  setSelectedDrives: (ids: string[]) => void;

  scan: ScanProgress | null;
  setScan: (p: ScanProgress) => void;

  actionDrives: string[];
  setActionDrives: (ids: string[]) => void;
  actionRunning: boolean;
  actionProgress: ActionProgress | null;
  actionResult: ActionResult | null;
  beginAction: () => void;
  setActionProgress: (p: ActionProgress) => void;
  finishAction: (r: ActionResult) => void;
  clearActionResult: () => void;

  reportBusy: boolean;
  reportMessages: string[];
  reportResult: ReportFiles | null;
  reportError: string | null;
  beginReport: () => void;
  pushReportMessage: (message: string) => void;
  finishReport: (result: ReportFiles | null, error: string | null) => void;
}

const saved = readSavedTheme();
const initialTheme: Theme = saved ?? systemTheme();
applyTheme(initialTheme); // before the first paint, so there is no flash of the wrong theme

export const useApp = create<AppState>()((set) => ({
  tab: 'drives',
  setTab: (tab) => set({ tab }),

  theme: initialTheme,
  savedChoice: saved !== null,
  toggleTheme: () =>
    set((s) => {
      const next: Theme = s.theme === 'dark' ? 'light' : 'dark';
      applyTheme(next);
      saveTheme(next);
      return { theme: next, savedChoice: true };
    }),
  followSystem: (theme) =>
    set((s) => {
      if (s.savedChoice) return s;
      applyTheme(theme);
      return { theme };
    }),

  selectedDrives: [],
  setSelectedDrives: (ids) => set({ selectedDrives: ids }),

  scan: null,
  setScan: (p) => set({ scan: p }),

  actionDrives: [],
  setActionDrives: (ids) => set({ actionDrives: ids }),
  actionRunning: false,
  actionProgress: null,
  actionResult: null,
  beginAction: () => set({ actionRunning: true, actionProgress: null, actionResult: null }),
  setActionProgress: (p) => set({ actionProgress: p, actionRunning: !p.finished && p.kind !== null }),
  finishAction: (r) => set({ actionResult: r, actionRunning: false }),
  clearActionResult: () => set({ actionResult: null }),

  reportBusy: false,
  reportMessages: [],
  reportResult: null,
  reportError: null,
  beginReport: () => set({ reportBusy: true, reportMessages: [], reportResult: null, reportError: null }),
  pushReportMessage: (message) =>
    set((s) => ({ reportMessages: [...s.reportMessages, message].slice(-MAX_REPORT_LINES) })),
  finishReport: (result, error) => set({ reportBusy: false, reportResult: result, reportError: error }),
}));
