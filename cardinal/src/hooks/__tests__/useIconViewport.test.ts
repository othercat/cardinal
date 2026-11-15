import { renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { expect, it, vi } from 'vitest';
import { useIconViewport } from '../useIconViewport';
import { FLAG_BITS } from '../../utils/eventFlags';
import type { SlabIndex } from '../../types/slab';
import type { SearchResultItem } from '../../types/search';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(undefined) }));

it('retains visible icons through writes and sorts, and reloads a changed preview, Finder icon or reused ID', () => {
  const ids = [1, 2, 3] as SlabIndex[];
  const rows = new Map<SlabIndex, SearchResultItem>([
    [ids[0], { path: '/root/log.tmp', size: 1 }],
    [ids[1], { path: '/root/image.other', size: 12, thumbnail: true }],
    [ids[2], { path: '/root/notes.md', size: 4 }],
  ]);
  const initial: Parameters<typeof useIconViewport>[0] = {
    results: ids,
    rows,
    queryVersion: 1,
    dataVersion: 1,
    pageVersion: 0,
    start: 0,
    end: 2,
    fileChanges: null,
  };
  const { rerender } = renderHook(useIconViewport, { initialProps: initial });
  const calls = vi.mocked(invoke);
  expect(calls).toHaveBeenLastCalledWith('update_icon_viewport', { id: 1, viewport: ids });
  const edit = (path: string, flagBits: number) => ({ path, flagBits, eventId: 1, timestamp: 1 });
  const newer = new Map(rows).set(ids[0], { path: '/root/log.tmp', size: 200 });
  rerender({
    ...initial,
    rows: newer,
    dataVersion: 2,
    results: [...ids].reverse(),
    fileChanges: [
      edit('/root/log.tmp', FLAG_BITS.ITEM_MODIFIED),
      edit('/not/visible', FLAG_BITS.ITEM_CREATED),
    ],
  });
  expect(calls).toHaveBeenCalledTimes(1);
  rerender({
    ...initial,
    rows: newer,
    dataVersion: 3,
    fileChanges: [
      edit('/root/image.other', FLAG_BITS.ITEM_MODIFIED),
      edit('/root/notes.md', FLAG_BITS.ITEM_FINDER_INFO_MOD),
    ],
  });
  expect(calls).toHaveBeenLastCalledWith('update_icon_viewport', { id: 2, viewport: ids.slice(1) });
  rerender({
    ...initial,
    rows: new Map(newer).set(ids[0], { path: '/root/replacement.bin' }),
    dataVersion: 4,
  });
  expect(calls).toHaveBeenLastCalledWith('update_icon_viewport', { id: 3, viewport: [ids[0]] });
});
