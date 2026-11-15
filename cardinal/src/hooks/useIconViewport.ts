import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { FileChangeBatch } from '../types/ipc';
import type { SearchResultItem } from '../types/search';
import type { SlabIndex } from '../types/slab';
import { FLAG_BITS } from '../utils/eventFlags';
import type { ResultPages } from '../utils/resultPages';

type Viewport = {
  results: SlabIndex[];
  pages?: ResultPages;
  rows: ReadonlyMap<SlabIndex, SearchResultItem>;
  queryVersion: number;
  dataVersion: number;
  pageVersion: number;
  fileChanges: FileChangeBatch;
  start: number;
  end: number;
};

const appearanceFlags =
  FLAG_BITS.ITEM_CREATED |
  FLAG_BITS.ITEM_REMOVED |
  FLAG_BITS.ITEM_RENAMED |
  FLAG_BITS.ITEM_CLONED |
  FLAG_BITS.ITEM_XATTR_MOD |
  FLAG_BITS.ITEM_FINDER_INFO_MOD;
const imageName =
  /\.(?:avif|bmp|dng|gif|heic|heif|ico|jpe?g|png|psd|svg|tiff?|webp|arw|cr[23]|nef|orf|raf|raw)$/i;

export function useIconViewport({
  results,
  pages,
  rows,
  queryVersion,
  dataVersion,
  pageVersion,
  fileChanges,
  start,
  end,
}: Viewport) {
  const visible = useRef(new Map<SlabIndex, SearchResultItem>());
  const lastQuery = useRef(-1);
  const lastData = useRef(-1);
  const requestId = useRef(0);

  useEffect(() => {
    if (lastQuery.current !== queryVersion) visible.current.clear();
    lastQuery.current = queryVersion;
    const changes = new Map<string, number>();
    if (lastData.current !== dataVersion) {
      for (const { path, flagBits } of fileChanges ?? []) {
        changes.set(path, (changes.get(path) ?? 0) | flagBits);
      }
    }
    lastData.current = dataVersion;

    const next = new Map<SlabIndex, SearchResultItem>();
    const needed: SlabIndex[] = [];
    const length = pages?.length ?? results.length;
    for (let i = Math.max(0, start); i <= end && i < length; i++) {
      const id = pages ? pages.idAt(i) : results[i];
      if (id === undefined) continue;
      const row = rows.get(id);
      if (!row) continue;
      next.set(id, row);
      const previous = visible.current.get(id);
      const flags = changes.get(row.path) ?? 0;
      const preview = row.thumbnail || imageName.test(row.path);
      if (
        previous?.path !== row.path ||
        previous.metadata?.type !== row.metadata?.type ||
        (flags & appearanceFlags) !== 0 ||
        (preview &&
          (changes.has(row.path) || previous.size !== row.size || previous.mtime !== row.mtime))
      ) {
        needed.push(id);
      }
    }
    visible.current = next;
    if (needed.length) {
      void invoke('update_icon_viewport', { id: ++requestId.current, viewport: needed }).catch(
        console.error,
      );
    }
  }, [results, pages, rows, queryVersion, dataVersion, pageVersion, fileChanges, start, end]);
}
