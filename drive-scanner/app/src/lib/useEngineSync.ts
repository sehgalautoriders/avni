import { useEffect, useRef } from 'react';
import { useQueryClient, type QueryClient } from '@tanstack/react-query';
import { api, type Phase } from '../api';
import { isScanActive, isScanFinished, useApp } from '../store';
import { qk } from './queries';

type Unlisten = () => void;

function refresh(qc: QueryClient, keys: readonly (readonly string[])[]): void {
  for (const queryKey of keys) void qc.invalidateQueries({ queryKey });
}

export const refreshAfterScan = (qc: QueryClient): void => refresh(qc, [qk.summary, qk.files, qk.dups, qk.drives]);
export const refreshAfterAction = (qc: QueryClient): void =>
  refresh(qc, [qk.summary, qk.files, qk.dups, qk.drives, qk.history, qk.finished, qk.preview]);

/**
 * The one place that talks to engine events. Call it once, at the top of the app:
 *  - subscribes to scan / action / report events once,
 *  - polls scan_status and action_status every second while something is running (fallback if an event is missed),
 *  - refreshes the cached reads when a scan or action finishes.
 */
export function useEngineSync(connected: boolean): void {
  const qc = useQueryClient();
  const scanActive = useApp((s) => isScanActive(s.scan));
  const actionRunning = useApp((s) => s.actionRunning);
  const phase = useApp((s) => s.scan?.phase);
  const previousPhase = useRef<Phase | undefined>(undefined);

  // Events: subscribe once. The promise may resolve after cleanup (React strict mode), so check `disposed`.
  useEffect(() => {
    if (!connected) return;
    let disposed = false;
    const unlisteners: Unlisten[] = [];
    const keep = (pending: Promise<Unlisten>): void => {
      pending
        .then((un) => {
          if (disposed) un();
          else unlisteners.push(un);
        })
        .catch(() => undefined);
    };
    const state = useApp.getState;

    keep(api.onScanProgress((p) => state().setScan(p)));
    keep(api.onScanDone(() => refreshAfterScan(qc)));
    keep(api.onActionProgress((p) => state().setActionProgress(p)));
    keep(
      api.onActionDone((r) => {
        state().finishAction(r);
        refreshAfterAction(qc);
      }),
    );
    keep(api.onReportProgress((m) => state().pushReportMessage(m)));

    // Pick up whatever is already happening (for example after the window was reloaded).
    api
      .scanStatus()
      .then((p) => state().setScan(p))
      .catch(() => undefined);
    api
      .actionStatus()
      .then((p) => state().setActionProgress(p))
      .catch(() => undefined);

    return () => {
      disposed = true;
      for (const un of unlisteners) un();
    };
  }, [connected, qc]);

  // Fallback: poll the scan every second while it is running.
  useEffect(() => {
    if (!connected || !scanActive) return;
    let stopped = false;
    const id = window.setInterval(() => {
      api
        .scanStatus()
        .then((p) => {
          if (!stopped) useApp.getState().setScan(p);
        })
        .catch(() => undefined); // the next tick tries again
    }, 1000);
    return () => {
      stopped = true;
      window.clearInterval(id);
    };
  }, [connected, scanActive]);

  // Whenever a scan reaches done / cancelled / failed, reload everything that depends on it.
  useEffect(() => {
    const before = previousPhase.current;
    previousPhase.current = phase;
    if (isScanFinished(phase) && before !== phase) refreshAfterScan(qc);
  }, [phase, qc]);

  // Fallback: poll the running action every second.
  useEffect(() => {
    if (!connected || !actionRunning) return;
    let stopped = false;
    const id = window.setInterval(() => {
      void (async () => {
        try {
          const p = await api.actionStatus();
          if (stopped) return;
          useApp.getState().setActionProgress(p);
          if (!p.finished) return;
          refreshAfterAction(qc);
          if (useApp.getState().actionResult !== null) return;
          // The "action-done" event was missed: take the newest entry from the history instead.
          const rows = await api.history();
          const latest = [...rows].sort((a, b) => b.started_ms - a.started_ms)[0];
          if (latest && useApp.getState().actionResult === null) useApp.getState().finishAction(latest);
        } catch {
          // the next tick tries again
        }
      })();
    }, 1000);
    return () => {
      stopped = true;
      window.clearInterval(id);
    };
  }, [connected, actionRunning, qc]);
}
