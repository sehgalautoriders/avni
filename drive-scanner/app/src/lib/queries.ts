import { useQuery } from '@tanstack/react-query';
import { api } from '../api';

/** Query keys for everything read from the engine. */
export const qk = {
  appInfo: ['appInfo'],
  drives: ['drives'],
  summary: ['summary'],
  settings: ['settings'],
  history: ['history'],
  reports: ['reports'],
  files: ['files'],
  dups: ['dups'],
  finished: ['finished'],
  preview: ['preview'],
  table: ['table'],
} as const;

export const useAppInfo = (enabled = true) =>
  useQuery({ queryKey: qk.appInfo, queryFn: () => api.appInfo(), enabled });

export const useDrives = () => useQuery({ queryKey: qk.drives, queryFn: () => api.listDrives() });

export const useSummary = () => useQuery({ queryKey: qk.summary, queryFn: () => api.getSummary() });

export const useSettings = () => useQuery({ queryKey: qk.settings, queryFn: () => api.getSettings() });

export const useHistory = () => useQuery({ queryKey: qk.history, queryFn: () => api.history() });

export const useReports = () => useQuery({ queryKey: qk.reports, queryFn: () => api.listReports() });
