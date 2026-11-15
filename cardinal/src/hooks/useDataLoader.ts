import type { ResultPages } from '../utils/resultPages';
import { useCallback, useRef, useEffect, useLayoutEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { subscribeIconUpdate } from '../runtime/tauriEventRuntime';
import type { NodeInfoResponse, SearchResultItem } from '../types/search';
import type { SlabIndex } from '../types/slab';
import type { IconUpdatePayload } from '../types/ipc';
import type { FileChangeBatch } from '../types/ipc';

export type DataLoaderCache = Map<SlabIndex, SearchResultItem>;
type IconOverrideValue = { icon?: string; path?: string; thumbnail?: boolean };

const fromNodeInfo = (node: NodeInfoResponse): SearchResultItem => ({
  path: node.path,
  metadata: node.metadata ?? undefined,
  size: node.size ?? node.metadata?.size,
  mtime: node.mtime ?? node.metadata?.mtime,
  ctime: node.ctime ?? node.metadata?.ctime,
  icon: node.icon ?? undefined,
});

// Data-only loader for visible rows. It owns row metadata caching and stale-request rejection;
// VirtualList handles any temporary frozen-view rendering during result-set swaps.
export function useDataLoader(
  results: SlabIndex[],
  dataResultsVersion: number,
  queryVersion = dataResultsVersion,
  fileChanges: FileChangeBatch = null,
  changedIds?: readonly number[],
  pages?: ResultPages,
) {
  const loadingRef = useRef(new Map<SlabIndex, symbol>());
  // Monotonic epoch for range-load requests. A new search result-set bumps this value so
  // late `get_nodes_info` responses from the previous result-set can be ignored safely.
  const versionRef = useRef(0);
  const cacheRef = useRef<DataLoaderCache>(new Map());
  const iconOverridesRef = useRef<Map<SlabIndex, IconOverrideValue>>(new Map());
  const staleRef = useRef(new Set<SlabIndex>());
  const queryVersionRef = useRef<number | null>(null);
  const [cache, setCache] = useState<DataLoaderCache>(() => {
    const initial = new Map<SlabIndex, SearchResultItem>();
    cacheRef.current = initial;
    return initial;
  });
  const [pageVersion, setPageVersion] = useState(0);
  const pagesRef = useRef(pages);
  pagesRef.current = pages;
  const resultsRef = useRef<SlabIndex[]>([]);
  resultsRef.current = results;

  // Reset cache state whenever the backing result-set changes so slab-index reuse in the
  // backend cannot surface stale row data for a newer search result-set.
  useLayoutEffect(() => {
    if (queryVersionRef.current === queryVersion && changedIds) {
      for (const id of changedIds) {
        loadingRef.current.delete(id as SlabIndex);
        if (cacheRef.current.has(id as SlabIndex)) staleRef.current.add(id as SlabIndex);
      }
      return;
    }
    versionRef.current += 1;
    loadingRef.current.clear();
    if (queryVersionRef.current !== queryVersion) {
      queryVersionRef.current = queryVersion;
      iconOverridesRef.current.clear();
      staleRef.current.clear();
      const nextCache = new Map<SlabIndex, SearchResultItem>();
      cacheRef.current = nextCache;
      setCache(nextCache);
    } else {
      // Keep painted rows during live updates. Only changed paths (including
      // descendants of renamed/scanned folders) need metadata revalidation.
      for (const [id, item] of cacheRef.current) {
        if (
          fileChanges === null ||
          fileChanges.some(
            ({ path }) =>
              item.path === path || item.path.startsWith(path === '/' ? path : `${path}/`),
          )
        ) {
          staleRef.current.add(id);
        }
      }
    }
  }, [dataResultsVersion, queryVersion, fileChanges, changedIds]);

  useEffect(() => {
    const unlistenIconUpdate = subscribeIconUpdate((updates: readonly IconUpdatePayload[]) => {
      if (updates.length === 0) {
        return;
      }

      {
        const prev = cacheRef.current;
        let nextCache: DataLoaderCache | null = null;

        updates.forEach((update) => {
          const slabIndex = update.slabIndex;
          const nextIcon = update.icon;
          const current = prev.get(slabIndex);
          if (current && update.path && current.path !== update.path) return;
          iconOverridesRef.current.set(slabIndex, {
            icon: nextIcon,
            path: update.path,
            thumbnail: update.thumbnail,
          });
          if (!current || (current.icon === nextIcon && current.thumbnail === update.thumbnail)) {
            return;
          }

          if (nextCache === null) {
            nextCache = new Map(prev);
          }

          nextCache.set(slabIndex, { ...current, icon: nextIcon, thumbnail: update.thumbnail });
        });

        if (nextCache !== null) {
          cacheRef.current = nextCache;
          setCache(nextCache);
        }
      }
    });
    return unlistenIconUpdate;
  }, []);

  const ensureRangeLoaded = useCallback(async (start: number, end: number) => {
    const list = resultsRef.current;
    const source = pagesRef.current;
    const total = source?.length ?? list.length;
    if (start < 0 || end < start || total === 0) return;
    const missingPage =
      source &&
      (source.idAt(start) === undefined || source.idAt(Math.min(end, total - 1)) === undefined);
    let ids: SlabIndex[];
    try {
      ids = source ? await source.range(start, end + 1) : list.slice(start, end + 1);
    } catch (error) {
      if (pagesRef.current === source)
        console.debug('File view advanced while loading a page', error);
      return;
    }
    if (source !== pagesRef.current) return;
    if (missingPage) setPageVersion((v) => v + 1);
    const needLoading: SlabIndex[] = [];
    const batch = Symbol();
    for (const slabIndex of ids) {
      // Request only cache misses in the active window.
      if (
        (!cacheRef.current.has(slabIndex) || staleRef.current.has(slabIndex)) &&
        !loadingRef.current.has(slabIndex)
      ) {
        needLoading.push(slabIndex);
        loadingRef.current.set(slabIndex, batch);
      }
    }
    if (needLoading.length === 0) return;
    const versionAtRequest = versionRef.current;
    let fetched: NodeInfoResponse[];
    try {
      fetched = await invoke<NodeInfoResponse[]>('get_nodes_info', {
        results: needLoading,
        includeIcons: false,
      });
    } catch (error) {
      for (const id of needLoading) {
        if (loadingRef.current.get(id) === batch) loadingRef.current.delete(id);
      }
      console.error('Loading row data failed:', error);
      return;
    }
    if (versionRef.current !== versionAtRequest) {
      // The result-set changed while this request was in flight. Drop the payload instead of
      // merging stale rows into the cache for the new query.
      return;
    }
    {
      const prev = cacheRef.current;
      let nextCache: DataLoaderCache | null = null;

      needLoading.forEach((slabIndex, idx) => {
        if (loadingRef.current.get(slabIndex) !== batch) return;
        const fetchedItem = fetched[idx];
        loadingRef.current.delete(slabIndex);
        staleRef.current.delete(slabIndex);
        if (!fetchedItem) {
          return;
        }

        const normalizedItem = fromNodeInfo(fetchedItem);
        const existing = prev.get(slabIndex);
        const samePath = existing?.path === normalizedItem.path;
        if (!samePath && iconOverridesRef.current.get(slabIndex)?.path !== normalizedItem.path)
          iconOverridesRef.current.delete(slabIndex);
        const hasOverride = iconOverridesRef.current.has(slabIndex);
        // Preserve newer icon updates that may have arrived after the node snapshot was read.
        const preferredIcon = hasOverride
          ? iconOverridesRef.current.get(slabIndex)?.icon
          : samePath
            ? (existing?.icon ?? normalizedItem.icon)
            : normalizedItem.icon;

        const thumbnail =
          iconOverridesRef.current.get(slabIndex)?.thumbnail ??
          (samePath ? existing?.thumbnail : undefined);
        const mergedItem = { ...normalizedItem, icon: preferredIcon, thumbnail };

        if (
          existing?.path === mergedItem.path &&
          existing.icon === mergedItem.icon &&
          existing.size === mergedItem.size &&
          existing.mtime === mergedItem.mtime &&
          existing.ctime === mergedItem.ctime &&
          existing.metadata?.type === mergedItem.metadata?.type &&
          existing.metadata?.size === mergedItem.metadata?.size &&
          existing.metadata?.mtime === mergedItem.metadata?.mtime &&
          existing.metadata?.ctime === mergedItem.metadata?.ctime
        )
          return;

        if (nextCache === null) {
          nextCache = new Map(prev);
        }

        nextCache.set(slabIndex, mergedItem);
      });

      if (nextCache !== null) {
        cacheRef.current = nextCache;
        setCache(nextCache);
      }
    }
  }, []);

  return { cache, ensureRangeLoaded, pageVersion };
}
