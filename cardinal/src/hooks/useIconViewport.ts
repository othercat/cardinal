import type { ResultPages } from '../utils/resultPages';
import { useCallback, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { SlabIndex } from '../types/slab';

type UseIconViewportProps = {
  results: SlabIndex[];
  pages?: ResultPages;
  resultsVersion: number;
  start: number;
  end: number;
};

// Deduplicates and throttles icon viewport updates to the backend.
export function useIconViewport({
  results,
  pages,
  resultsVersion,
  start,
  end,
}: UseIconViewportProps) {
  const requestIdRef = useRef(0);
  const lastRangeRef = useRef<{ start: number; end: number } | null>(null);
  const pendingRef = useRef<SlabIndex[] | null>(null);
  const rafRef = useRef<number | null>(null);

  const flushIconViewport = useCallback(() => {
    rafRef.current = null;
    const viewport = pendingRef.current;
    if (!viewport) return;
    pendingRef.current = null;
    requestIdRef.current += 1;
    void invoke('update_icon_viewport', { id: requestIdRef.current, viewport });
  }, []);

  const scheduleIconViewport = useCallback(
    (viewport: SlabIndex[]) => {
      pendingRef.current = viewport;
      if (rafRef.current === null) {
        rafRef.current = requestAnimationFrame(flushIconViewport);
      }
    },
    [flushIconViewport],
  );

  useEffect(() => {
    // Reset tracking when the source list identity changes.
    lastRangeRef.current = null;
  }, [resultsVersion]);

  useEffect(() => {
    const length = pages?.length ?? results.length;
    const clampedStart = Math.max(0, start);
    const clampedEnd = Math.min(end, length - 1);
    const hasRange = length > 0 && end >= start && clampedEnd >= clampedStart;

    // Nothing to send; emit a clear once per empty state.
    if (!hasRange) {
      if (!lastRangeRef.current || lastRangeRef.current.start !== -1) {
        lastRangeRef.current = { start: -1, end: -1 };
        scheduleIconViewport([]);
      }
      return;
    }

    const last = lastRangeRef.current;
    if (last && last.start === clampedStart && last.end === clampedEnd) {
      return; // viewport unchanged; skip IPC
    }

    lastRangeRef.current = { start: clampedStart, end: clampedEnd };
    if (!pages) {
      scheduleIconViewport(results.slice(clampedStart, clampedEnd + 1));
      return;
    }
    let stale = false;
    void pages
      .range(clampedStart, clampedEnd + 1)
      .then((ids) => {
        if (!stale) scheduleIconViewport(ids);
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [results, pages, resultsVersion, start, end, scheduleIconViewport]);

  useEffect(
    () => () => {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
      }
      pendingRef.current = [];
      flushIconViewport();
    },
    [flushIconViewport],
  );
}
