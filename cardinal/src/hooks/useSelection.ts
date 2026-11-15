import type { ResultPages } from '../utils/resultPages';
import { useCallback, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { MutableRefObject, RefObject } from 'react';
import type { VirtualListHandle } from '../components/VirtualList';
import type { SlabIndex } from '../types/slab';
import type { FileChangeBatch } from '../types/ipc';
import { FLAG_BITS } from '../utils/eventFlags';

type RowSelectOptions = {
  isShift: boolean;
  isMeta: boolean;
  isCtrl: boolean;
};

export type SelectionController = {
  selectedIndices: number[];
  selectedIndicesRef: MutableRefObject<number[]>;
  activeRowIndex: number | null;
  shiftAnchorIndex: number | null;
  selectedPaths: string[];
  handleRowSelect: (rowIndex: number, options: RowSelectOptions) => void;
  selectSingleRow: (rowIndex: number) => void;
  clearSelection: () => void;
  moveSelection: (delta: 1 | -1, options?: { extend?: boolean }) => void;
};

/**
 * Manages virtualized row selection. Keeps indexes/anchors in sync with the rendered list,
 * exposes helpers for shift/meta selection, and remaps selections when the backing data changes.
 * The hook also tracks the concrete paths backing the selection so consumers can interact with
 * context menus, Quick Look, etc. without reimplementing bookkeeping.
 * Without live options a new resultsVersion resets selection. Live updates remap
 * row positions by ID, retaining captured paths and rejecting removed/renamed paths.
 */
export const useSelection = (
  displayedResults: SlabIndex[],
  resultsVersion: number,
  virtualListRef: RefObject<VirtualListHandle | null>,
  live?: {
    queryVersion: number;
    dataVersion: number;
    fileChanges: FileChangeBatch;
    pages?: ResultPages;
  },
): SelectionController => {
  const [selectedIndices, setSelectedIndices] = useState<number[]>([]);
  const [activeRowIndex, setActiveRowIndex] = useState<number | null>(null);
  const [shiftAnchorIndex, setShiftAnchorIndex] = useState<number | null>(null);
  const selectedIndicesRef = useRef<number[]>([]);
  const pages = live?.pages;
  const length = pages?.length ?? displayedResults.length;
  const capturedRows = useRef(new Map<number, SlabIndex>());
  const remapGeneration = useRef(0);
  const remapping = useRef(false);
  const resetVersion = live?.queryVersion ?? resultsVersion;
  const dataVersion = live?.dataVersion ?? resultsVersion;
  const fileChanges = live?.fileChanges;
  const sourceRef = useRef({
    results: displayedResults,
    pages,
    resetVersion,
    dataVersion,
    resultsVersion,
  });
  const selectedPathsRef = useRef(new Map<SlabIndex, string>());

  const handleRowSelect = useCallback(
    (rowIndex: number, options: RowSelectOptions) => {
      remapGeneration.current++;
      remapping.current = false;
      const { isShift, isMeta, isCtrl } = options;
      const isCmdOrCtrl = isMeta || isCtrl;

      if (isShift && shiftAnchorIndex !== null) {
        const start = Math.min(shiftAnchorIndex, rowIndex);
        const end = Math.max(shiftAnchorIndex, rowIndex);
        const range: number[] = [];
        for (let i = start; i <= end; i += 1) {
          range.push(i);
        }
        setSelectedIndices(range);
      } else if (isCmdOrCtrl) {
        setSelectedIndices((prevIndices) => {
          const isDeselecting = prevIndices.includes(rowIndex);
          const nextIndices = isDeselecting
            ? prevIndices.filter((index) => index !== rowIndex)
            : [...prevIndices, rowIndex];

          // Handle shift anchor updates
          if (isDeselecting) {
            // If deselecting, find the next closest selected item below it as the new anchor
            let newAnchor: number | null = null;
            for (let i = rowIndex + 1; i < length; i += 1) {
              if (nextIndices.includes(i)) {
                newAnchor = i;
                break;
              }
            }
            // If nothing below, look upward
            if (newAnchor === null) {
              for (let i = rowIndex - 1; i >= 0; i -= 1) {
                if (nextIndices.includes(i)) {
                  newAnchor = i;
                  break;
                }
              }
            }
            setShiftAnchorIndex(newAnchor);
          } else if (!isDeselecting) {
            // Only update anchor when adding (not removing) an item
            setShiftAnchorIndex(rowIndex);
          }
          // If deselecting a non-anchor item, keep the anchor unchanged

          return nextIndices;
        });
      } else {
        setSelectedIndices([rowIndex]);
        setShiftAnchorIndex(rowIndex);
      }

      setActiveRowIndex(rowIndex);
    },
    [shiftAnchorIndex, length],
  );

  const selectSingleRow = useCallback((rowIndex: number) => {
    remapGeneration.current++;
    remapping.current = false;
    setSelectedIndices([rowIndex]);
    setActiveRowIndex(rowIndex);
    setShiftAnchorIndex(rowIndex);
  }, []);

  const clearSelection = useCallback(() => {
    remapGeneration.current++;
    remapping.current = false;
    capturedRows.current.clear();
    setSelectedIndices((prev) => (prev.length === 0 ? prev : []));
    setActiveRowIndex((prev) => (prev === null ? prev : null));
    setShiftAnchorIndex((prev) => (prev === null ? prev : null));
  }, []);

  const moveSelection = useCallback(
    (delta: 1 | -1, options?: { extend?: boolean }) => {
      if (length === 0) {
        return;
      }

      const fallbackIndex = delta > 0 ? -1 : length;
      const baseIndex = activeRowIndex ?? fallbackIndex;
      const nextIndex = Math.min(Math.max(baseIndex + delta, 0), length - 1);

      if (nextIndex === activeRowIndex) {
        return;
      }

      const item = virtualListRef.current?.getItem(nextIndex);
      if (!item) {
        return;
      }

      handleRowSelect(nextIndex, {
        isShift: options?.extend ?? false,
        isMeta: false,
        isCtrl: false,
      });
    },
    [activeRowIndex, length, handleRowSelect],
  );

  useLayoutEffect(() => {
    const previous = sourceRef.current;
    sourceRef.current = {
      results: displayedResults,
      pages,
      resetVersion,
      dataVersion,
      resultsVersion,
    };
    if (previous.resetVersion !== resetVersion) {
      clearSelection();
      selectedPathsRef.current.clear();
      return;
    }
    if (
      previous.results === displayedResults &&
      previous.pages === pages &&
      previous.dataVersion === dataVersion &&
      previous.resultsVersion === resultsVersion
    )
      return;
    if (previous.dataVersion !== dataVersion && fileChanges === null) {
      // A rescan/overflow can assign an old slab ID to another path. Without
      // a path-level delta there is no safe identity to retain.
      clearSelection();
      selectedPathsRef.current.clear();
      return;
    }
    if (selectedIndices.length === 0 && activeRowIndex === null && shiftAnchorIndex === null)
      return;

    const removed = (id: SlabIndex) => {
      if (previous.dataVersion === dataVersion) return false;
      const path = selectedPathsRef.current.get(id);
      return (
        path !== undefined &&
        fileChanges?.some(
          (event) =>
            path.startsWith(event.path === '/' ? '/' : `${event.path}/`) ||
            (path === event.path &&
              (event.flagBits === 0 ||
                (event.flagBits &
                  (FLAG_BITS.ITEM_REMOVED |
                    FLAG_BITS.ITEM_RENAMED |
                    FLAG_BITS.ITEM_IS_DIR |
                    FLAG_BITS.MUST_SCAN_SUBDIRS)) !==
                  0)),
        )
      );
    };
    const previousId = (index: number) =>
      previous.pages
        ? (previous.pages.idAt(index) ?? capturedRows.current.get(index))
        : previous.results[index];
    const selectedIds = selectedIndices
      .map(previousId)
      .filter((id): id is SlabIndex => id !== undefined && !removed(id));
    const activeId = activeRowIndex === null ? undefined : previousId(activeRowIndex);
    const anchorId = shiftAnchorIndex === null ? undefined : previousId(shiftAnchorIndex);
    if (
      previous.results === displayedResults &&
      previous.pages === pages &&
      selectedIds.length === selectedIndices.length &&
      (activeId === undefined || !removed(activeId)) &&
      (anchorId === undefined || !removed(anchorId))
    )
      return;
    const wanted = new Set(
      [...selectedIds, activeId, anchorId].filter(
        (id): id is SlabIndex => id !== undefined && !removed(id),
      ),
    );
    const applyPositions = (positions: Map<SlabIndex, number>) => {
      remapping.current = false;
      const next = selectedIds
        .map((id) => positions.get(id))
        .filter((index): index is number => index !== undefined);
      setSelectedIndices((previousIndices) =>
        previousIndices.length === next.length &&
        previousIndices.every((value, index) => value === next[index])
          ? previousIndices
          : next,
      );
      setActiveRowIndex(activeId === undefined ? null : (positions.get(activeId) ?? null));
      setShiftAnchorIndex(anchorId === undefined ? null : (positions.get(anchorId) ?? null));
      capturedRows.current = new Map([...positions].map(([id, index]) => [index, id]));
    };
    const generation = ++remapGeneration.current;
    if (pages) {
      remapping.current = true;
      void pages
        .positions([...wanted])
        .then((positions) => {
          if (generation === remapGeneration.current) applyPositions(positions);
        })
        .catch(() => {});
    } else {
      const positions = new Map<SlabIndex, number>();
      displayedResults.forEach((id, index) => {
        if (wanted.has(id)) positions.set(id, index);
      });
      applyPositions(positions);
    }
  }, [
    displayedResults,
    pages,
    resultsVersion,
    resetVersion,
    dataVersion,
    fileChanges,
    selectedIndices,
    activeRowIndex,
    shiftAnchorIndex,
    clearSelection,
  ]);

  useLayoutEffect(() => {
    selectedIndicesRef.current = selectedIndices;
    if (remapping.current) return;
    const paths = new Map<SlabIndex, string>();
    for (const index of selectedIndices) {
      const id = pages
        ? (pages.idAt(index) ?? capturedRows.current.get(index))
        : displayedResults[index];
      if (id === undefined) continue;
      capturedRows.current.set(index, id);
      const path = selectedPathsRef.current.get(id) ?? virtualListRef.current?.getItem(index)?.path;
      if (id !== undefined && path) paths.set(id, path);
    }
    // Retain paths while native selection ranks are being resolved.
    selectedPathsRef.current = paths;
    for (const row of capturedRows.current.keys())
      if (!selectedIndices.includes(row)) capturedRows.current.delete(row);
  }, [selectedIndices, displayedResults, pages, virtualListRef]);

  const selectedPaths = useMemo(() => {
    const list = virtualListRef.current;
    if (!list) {
      return [];
    }
    const paths: string[] = [];
    selectedIndices.forEach((index) => {
      const item = list.getItem(index);
      const path =
        selectedPathsRef.current.get(
          (pages ? capturedRows.current.get(index) : displayedResults[index]) as SlabIndex,
        ) ?? item?.path;
      if (path) paths.push(path);
    });
    return paths;
  }, [selectedIndices, displayedResults, pages, virtualListRef]);

  return {
    selectedIndices,
    selectedIndicesRef,
    activeRowIndex,
    shiftAnchorIndex,
    selectedPaths,
    handleRowSelect,
    selectSingleRow,
    clearSelection,
    moveSelection,
  };
};
