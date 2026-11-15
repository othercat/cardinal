import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { MutableRefObject } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { SortKey, SortState } from '../types/sort';
import type { SlabIndex } from '../types/slab';
import type { FileChangeBatch, SortedEdits } from '../types/ipc';
import { applyResultEdits } from '../utils/resultEdits';
import { ResultPages } from '../utils/resultPages';
import { sortedWindow, type LoadedWindow } from '../utils/sortLoadedRows';

const sameIds = (a: SlabIndex[], b: SlabIndex[]): boolean =>
  a === b || (a.length === b.length && a.every((id, index) => id === b[index]));

const SORT_THRESHOLD_STORAGE_KEY = 'cardinal.sortThreshold';
const SORT_THRESHOLD_MIGRATED_KEY = 'cardinal.sortThreshold.v2';
// 0 means unlimited; the old default worked around per-result stat calls.
export const DEFAULT_SORTABLE_RESULT_THRESHOLD = 0;

let lastSortRequestId = 0;
const nextSortRequestId = (): number => {
  // Remains monotonic across hook remounts and WebView reloads. Microsecond-scale
  // integer values are within JavaScript's safe range for current wall clocks.
  lastSortRequestId = Math.max(lastSortRequestId + 1, Date.now() * 1000);
  return lastSortRequestId;
};

const clampSortThreshold = (value: number): number => {
  if (!Number.isFinite(value)) {
    return DEFAULT_SORTABLE_RESULT_THRESHOLD;
  }
  const rounded = Math.round(value);
  return Math.max(0, rounded);
};

const readStoredSortThreshold = (): number => {
  if (typeof window === 'undefined') {
    return DEFAULT_SORTABLE_RESULT_THRESHOLD;
  }
  let stored: string | null;
  let migrated: boolean;
  try {
    stored = window.localStorage.getItem(SORT_THRESHOLD_STORAGE_KEY);
    migrated = window.localStorage.getItem(SORT_THRESHOLD_MIGRATED_KEY) === 'true';
  } catch {
    return DEFAULT_SORTABLE_RESULT_THRESHOLD;
  }
  if (stored == null) {
    return DEFAULT_SORTABLE_RESULT_THRESHOLD;
  }
  const parsed = Number.parseInt(stored, 10);
  if (Number.isNaN(parsed)) {
    return DEFAULT_SORTABLE_RESULT_THRESHOLD;
  }
  if (parsed === 20000 && !migrated) {
    persistSortThreshold(DEFAULT_SORTABLE_RESULT_THRESHOLD);
    return DEFAULT_SORTABLE_RESULT_THRESHOLD;
  }
  return clampSortThreshold(parsed);
};

const persistSortThreshold = (value: number): void => {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.localStorage.setItem(SORT_THRESHOLD_STORAGE_KEY, String(value));
    window.localStorage.setItem(SORT_THRESHOLD_MIGRATED_KEY, 'true');
  } catch {
    // Ignore storage failures.
  }
};

export type RemoteSortControls = {
  sortState: SortState;
  displayedResults: SlabIndex[];
  displayedPages?: ResultPages;
  // Monotonic token for consumers that must invalidate when visible rows/order changes.
  displayedResultsVersion: number;
  displayedDataVersion: number;
  displayedFileChanges: FileChangeBatch;
  displayedChangedIds?: readonly number[];
  sortThreshold: number;
  setSortThreshold: (value: number) => void;
  canSort: boolean;
  isSorting: boolean;
  sortDisabledTooltip: string | null;
  sortButtonsDisabled: boolean;
  handleSortToggle: (key: SortKey) => void;
};

export const useRemoteSort = (
  results: SlabIndex[],
  resultsVersion: number,
  locale: string,
  formatDisabledTooltip: (limit: string) => string | null,
  options?: {
    queryVersion: number;
    fileChanges: FileChangeBatch;
    viewId?: number;
    pagedCount?: number;
    sortedEdits?: SortedEdits;
    changedIds?: readonly number[];
    getLoadedWindow?: () => LoadedWindow | null;
    activeSortKey?: MutableRefObject<SortKey | undefined>;
    pendingSortRef?: MutableRefObject<Promise<unknown> | null>;
  },
): RemoteSortControls => {
  const queryVersion = options?.queryVersion ?? resultsVersion;
  const fileChanges = options?.fileChanges ?? null;
  const viewId = options?.viewId;
  const pagedCount = options?.pagedCount;
  const changedIds = options?.changedIds;
  const sortedEdits = options?.sortedEdits;
  const getLoadedWindow = options?.getLoadedWindow;
  const localPendingSortRef = useRef<Promise<unknown> | null>(null);
  const pendingSortRef = options?.pendingSortRef ?? localPendingSortRef;
  const [sortState, setSortState] = useState<SortState>(null);
  type CompletedView = {
    key: SortKey;
    direction: string;
    sourceId?: number;
    pagedCount?: number;
    changedIds?: readonly number[];
    version: number;
    ordered: SlabIndex[];
    queryVersion: number;
    fileChanges: FileChangeBatch;
  };
  const [sortedView, setSortedView] = useState<CompletedView | null>(null);
  const completed = useRef<CompletedView | null>(null);
  const [preview, setPreview] = useState<{
    ordered: SlabIndex[];
    sort: NonNullable<SortState>;
    version: number;
    complete: boolean;
  } | null>(null);
  const [sortThreshold, setSortThresholdState] = useState<number>(() => readStoredSortThreshold());
  const [isSorting, setIsSorting] = useState(false);
  const sortRequestRef = useRef(0);
  const canSort = sortThreshold === 0 || (pagedCount ?? results.length) <= sortThreshold;
  const activeSortKey = options?.activeSortKey;
  useEffect(() => {
    if (activeSortKey) activeSortKey.current = canSort ? sortState?.key : undefined;
  }, [sortState, canSort, activeSortKey]);
  const shouldUseSortedResults = Boolean(
    sortState && canSort && results.length > 0 && sortedView?.queryVersion === queryVersion,
  );
  const currentOrder =
    shouldUseSortedResults &&
    sortedView!.version === resultsVersion &&
    sortedView!.key === sortState!.key;
  const direction = sortState?.direction;
  const finished = useMemo(() => {
    if (!sortedView) return null;
    return currentOrder &&
      direction !== sortedView.direction &&
      sortedView.pagedCount === undefined &&
      viewId !== undefined
      ? sortedView.ordered.slice().reverse()
      : sortedView.ordered;
  }, [currentOrder, direction, sortedView, viewId]);
  const usePreview =
    preview &&
    sortState &&
    canSort &&
    preview.version === resultsVersion &&
    preview.sort === sortState &&
    !(currentOrder && (viewId !== undefined || sortedView!.direction === direction));
  const displayedResults = usePreview
    ? preview.ordered
    : shouldUseSortedResults
      ? finished!
      : results;
  const displayedDataVersion =
    shouldUseSortedResults && !usePreview ? sortedView!.version : resultsVersion;
  const displayedFileChanges =
    shouldUseSortedResults && !usePreview ? sortedView!.fileChanges : fileChanges;
  const sourceId = shouldUseSortedResults && !usePreview ? sortedView!.sourceId : viewId;
  const total = shouldUseSortedResults && !usePreview ? sortedView!.pagedCount : pagedCount;
  const pageKey = shouldUseSortedResults && !usePreview ? sortedView!.key : undefined;
  const pageDirection = shouldUseSortedResults && !usePreview ? sortedView!.direction : 'asc';
  const displayedPages = useMemo(
    () =>
      total !== undefined && sourceId !== undefined
        ? new ResultPages(
            sourceId,
            total,
            pageKey ? { key: pageKey, direction: pageDirection === 'desc' ? 'desc' : 'asc' } : null,
            displayedResults,
          )
        : undefined,
    [sourceId, total, pageKey, pageDirection, displayedResults],
  );
  const [projection, setProjection] = useState({
    ids: results,
    pages: displayedPages,
    queryVersion,
    version: 1,
  });
  if (
    projection.queryVersion !== queryVersion ||
    projection.pages !== displayedPages ||
    !sameIds(projection.ids, displayedResults)
  ) {
    // Adjust before committing children: order changes and viewport epochs must
    // arrive together, without an intermediate unsorted/placeholder frame.
    setProjection({
      ids: displayedResults,
      pages: displayedPages,
      queryVersion,
      version: projection.version + 1,
    });
  }
  const displayedResultsVersion = projection.version;

  const setSortThreshold = useCallback((value: number) => {
    const normalized = clampSortThreshold(value);
    setSortThresholdState(normalized);
    persistSortThreshold(normalized);
  }, []);

  const handleSortToggle = useCallback(
    (nextKey: SortKey) => {
      if (!canSort) {
        return;
      }
      const next: SortState =
        sortState?.key !== nextKey
          ? { key: nextKey, direction: 'asc' }
          : sortState.direction === 'asc'
            ? { key: nextKey, direction: 'desc' }
            : null;
      let nextPreview = null;
      if (next && !(currentOrder && sortedView?.key === nextKey && viewId !== undefined)) {
        const window = getLoadedWindow?.();
        if (
          pagedCount === undefined &&
          window &&
          window.start === 0 &&
          window.rows.length === displayedResults.length &&
          window.rows.every((row, i) => displayedResults[window.start + i] === row.id)
        ) {
          const ids = sortedWindow(window, nextKey, next.direction === 'desc');
          if (ids) {
            nextPreview = { ordered: ids, sort: next, version: resultsVersion, complete: true };
          }
        }
      }
      setPreview(nextPreview);
      setSortState(next);
    },
    [
      canSort,
      sortState,
      currentOrder,
      sortedView,
      viewId,
      getLoadedWindow,
      pagedCount,
      displayedResults,
      resultsVersion,
    ],
  );

  useEffect(() => {
    const requestId = sortRequestRef.current + 1;
    sortRequestRef.current = requestId;

    if (!sortState || !canSort || results.length === 0) {
      if (pendingSortRef.current) {
        void invoke('get_sorted_view', {
          results: [],
          sort: null,
          requestId: nextSortRequestId(),
        }).catch(() => {});
      }
      setIsSorting(false);
      setPreview(null);
      completed.current = null;
      setSortedView(null);
      return;
    }

    const previous = completed.current;
    if (
      viewId !== undefined &&
      previous?.sourceId === viewId &&
      previous.key === sortState.key &&
      (pagedCount === undefined || previous.direction === sortState.direction)
    ) {
      setIsSorting(false);
      return;
    }
    const publish = (ordered: SlabIndex[], canonical: boolean) => {
      const next = {
        version: resultsVersion,
        key: sortState.key,
        direction: canonical ? 'asc' : sortState.direction,
        sourceId: viewId,
        pagedCount,
        queryVersion,
        fileChanges,
        changedIds,
        ordered:
          previous?.queryVersion === queryVersion && sameIds(previous.ordered, ordered)
            ? previous.ordered
            : ordered,
      };
      completed.current = next;
      setPreview(null);
      setSortedView(next);
      setIsSorting(false);
    };
    if (
      viewId !== undefined &&
      previous &&
      pagedCount === undefined &&
      previous.pagedCount === undefined &&
      sortedEdits &&
      sortedEdits.key === sortState.key &&
      previous.key === sortState.key &&
      previous.sourceId === sortedEdits.baseViewId
    ) {
      publish(applyResultEdits(previous.ordered, sortedEdits.edits), true);
      return;
    }
    setIsSorting(true);
    const task = (async () => {
      try {
        const canonical = viewId !== undefined && pagedCount === undefined;
        const response = await invoke<number[] | ArrayBuffer>('get_sorted_view', {
          ...(viewId !== undefined ? { viewId } : { results }),
          ...(pagedCount === undefined ? {} : { paged: true }),
          sort: canonical ? { key: sortState.key, direction: 'asc' } : sortState,
          requestId: nextSortRequestId(),
        });
        if (sortRequestRef.current === requestId) {
          const ordered = Array.isArray(response)
            ? response
            : Array.from(new Uint32Array(response));
          publish(ordered as SlabIndex[], canonical);
        }
      } catch (error) {
        if (sortRequestRef.current === requestId) {
          setPreview(null);
          console.error('Sorting failed:', error);
          // Keep the last completed view while a live update is interrupted.
        }
      } finally {
        if (sortRequestRef.current === requestId) {
          setIsSorting(false);
        }
      }
    })();
    pendingSortRef.current = task;
    void task.finally(() => {
      if (pendingSortRef.current === task) pendingSortRef.current = null;
    });
    return () => {
      sortRequestRef.current += 1;
    };
  }, [
    results,
    resultsVersion,
    queryVersion,
    fileChanges,
    sortState,
    canSort,
    pendingSortRef,
    viewId,
    sortedEdits,
    changedIds,
    pagedCount,
  ]);

  const sortLimitLabel = useMemo(
    () => new Intl.NumberFormat(locale).format(sortThreshold),
    [locale, sortThreshold],
  );
  const sortDisabledTooltip = canSort ? null : formatDisabledTooltip(sortLimitLabel);
  const sortButtonsDisabled = !canSort;

  return {
    sortState,
    displayedResults,
    displayedPages,
    displayedResultsVersion,
    displayedDataVersion,
    displayedFileChanges,
    displayedChangedIds:
      shouldUseSortedResults && !usePreview ? sortedView!.changedIds : changedIds,
    sortThreshold,
    setSortThreshold,
    canSort,
    isSorting: isSorting && !(usePreview && preview.complete),
    sortDisabledTooltip,
    sortButtonsDisabled,
    handleSortToggle,
  };
};
