import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import type { SlabIndex } from '../../types/slab';
import { ResultPages } from '../resultPages';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const ids = (offset: number, length: number) =>
  Array.from({ length }, (_, i) => (i + offset) as SlabIndex);

describe('ResultPages', () => {
  it('uses the initial page and shares a request crossing two viewport ranges', async () => {
    const transport = vi.mocked(invoke).mockReset();
    transport.mockResolvedValueOnce(ids(256, 44));
    const pages = new ResultPages(42, 300, { key: 'size', direction: 'desc' }, ids(0, 256));
    const [across, tail] = await Promise.all([pages.range(250, 270), pages.range(290, 400)]);
    expect(across).toEqual(ids(250, 20));
    expect(tail).toEqual(ids(290, 10));
    expect(transport).toHaveBeenCalledTimes(1);
    expect(transport).toHaveBeenCalledWith('get_view_page', {
      viewId: 42,
      sort: { key: 'size', direction: 'desc' },
      offset: 256,
      limit: 256,
    });
    expect(pages.idAt(299)).toBe(299);
  });

  it('retries a stale page and skips IDs absent from the current view', async () => {
    const transport = vi.mocked(invoke).mockReset();
    transport.mockResolvedValueOnce([]).mockResolvedValueOnce(ids(256, 2));
    const pages = new ResultPages(1, 258, null, ids(0, 256));
    await expect(pages.range(256, 258)).rejects.toThrow();
    expect(await pages.range(256, 258)).toEqual([256, 257]);
    transport.mockResolvedValueOnce([67, 0xffffffff]);
    expect(await pages.positions(ids(20, 2))).toEqual(new Map([[20, 67]]));
  });
});
