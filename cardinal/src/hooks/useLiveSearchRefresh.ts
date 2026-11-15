import { useEffect, useRef } from 'react';
import { subscribeIndexedEvents } from '../runtime/tauriEventRuntime';
import { useStableEvent } from './useStableEvent';
import type { FileChangeBatch, RecentEventPayload } from '../types/ipc';

export const LIVE_SEARCH_REFRESH_MS = 32;
const FULL_QUERY_INTERVAL_MS = 500;

// At most one refresh in flight. The first event starts a fixed window, so a
// continuous stream cannot postpone refresh forever as a trailing debounce can.
export function useLiveSearchRefresh(
  refresh: (changes: FileChangeBatch) => Promise<unknown>,
  ready: boolean,
  incremental = true,
  scanProgress?: number | string,
): void {
  const onScanProgress = useRef<(() => void) | null>(null);
  const refreshLatest = useStableEvent(refresh);
  const interval = useStableEvent(() =>
    incremental ? LIVE_SEARCH_REFRESH_MS : FULL_QUERY_INTERVAL_MS,
  );

  useEffect(() => {
    if (!ready) return;
    let dirty = true; // Also refresh after startup/history replay or a rescan.
    let fullRefresh = true;
    const pending = new Map<string, RecentEventPayload>();
    let running = false;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const collect = (events: FileChangeBatch) => {
      if (events === null) fullRefresh = true;
      if (fullRefresh) {
        pending.clear();
        return;
      }
      for (const event of events ?? []) {
        const previous = pending.get(event.path);
        pending.set(event.path, { ...event, flagBits: event.flagBits | (previous?.flagBits ?? 0) });
      }
      if (pending.size > 2048) {
        fullRefresh = true;
        pending.clear();
      }
    };

    const schedule = (delay = interval()) => {
      if (disposed || !dirty || running || timer !== null || document.hidden) return;
      timer = setTimeout(async () => {
        timer = null;
        if (disposed || document.hidden) return;
        dirty = false;
        const changes: FileChangeBatch = fullRefresh ? null : [...pending.values()];
        fullRefresh = false;
        pending.clear();
        running = true;
        let nextDelay: number | undefined;
        try {
          const consumed = await refreshLatest(changes);
          if (consumed === false) {
            dirty = true;
            collect(changes);
            nextDelay = Math.max(interval(), 100);
          }
        } catch (error) {
          console.error('Live search refresh failed:', error);
          dirty = true;
          fullRefresh = true;
          nextDelay = 1000;
        } finally {
          running = false;
          schedule(nextDelay);
        }
      }, delay);
    };

    onScanProgress.current = () => {
      dirty = true;
      schedule();
    };
    const unlisten = subscribeIndexedEvents((events) => {
      if (events.length === 0) return;
      dirty = true;
      collect(events);
      schedule();
    });
    const onVisibilityChange = () => {
      if (document.hidden && timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      schedule(0);
    };
    document.addEventListener('visibilitychange', onVisibilityChange);
    window.addEventListener('focus', onVisibilityChange);
    schedule();

    return () => {
      disposed = true;
      onScanProgress.current = null;
      unlisten();
      if (timer !== null) clearTimeout(timer);
      document.removeEventListener('visibilitychange', onVisibilityChange);
      window.removeEventListener('focus', onVisibilityChange);
    };
  }, [ready, refreshLatest, interval]);

  useEffect(() => {
    if (scanProgress !== undefined) onScanProgress.current?.();
  }, [scanProgress]);
}
