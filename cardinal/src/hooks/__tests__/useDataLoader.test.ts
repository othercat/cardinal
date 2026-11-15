import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { subscribeIconUpdate } from '../../runtime/tauriEventRuntime';
import type { SlabIndex } from '../../types/slab';
import { useDataLoader } from '../useDataLoader';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('../../runtime/tauriEventRuntime', () => ({
  subscribeIconUpdate: vi.fn(),
}));

const mockedInvoke = vi.mocked(invoke);
const mockedSubscribeIconUpdate = vi.mocked(subscribeIconUpdate);
type HookProps = { results: SlabIndex[]; version: number };

const buildNodeInfo = (slabIndex: SlabIndex) => ({
  path: `/tmp/file-${slabIndex}`,
  icon: null,
  metadata: null,
  size: null,
  mtime: null,
  ctime: null,
});
type BuiltNodeInfo = ReturnType<typeof buildNodeInfo>;

const renderDataLoader = (initialProps: HookProps) =>
  renderHook(({ results, version }: HookProps) => useDataLoader(results, version), {
    initialProps,
  });

const createDeferred = <T>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
};

describe('useDataLoader', () => {
  const iconUpdateUnlisten = vi.fn();

  beforeEach(() => {
    mockedSubscribeIconUpdate.mockImplementation(() => iconUpdateUnlisten);
    mockedInvoke.mockImplementation((command: string, payload?: unknown) => {
      if (command !== 'get_nodes_info') {
        return Promise.resolve(null);
      }

      const slabIndices = (payload as { results: SlabIndex[] }).results;
      return Promise.resolve(slabIndices.map((slabIndex) => buildNodeInfo(slabIndex)));
    });
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('does not clear cache when only results reference changes', async () => {
    const slab11 = 11 as SlabIndex;
    const slab22 = 22 as SlabIndex;
    const first = [slab11, slab22];
    const { result, rerender } = renderDataLoader({ results: first, version: 1 });

    await act(async () => {
      await result.current.ensureRangeLoaded(0, 1);
    });

    await waitFor(() => {
      expect(result.current.cache.get(slab11)?.path).toBe('/tmp/file-11');
      expect(result.current.cache.get(slab22)?.path).toBe('/tmp/file-22');
    });
    expect(mockedInvoke).toHaveBeenCalledTimes(1);

    rerender({ results: [...first], version: 1 });

    await act(async () => {
      await result.current.ensureRangeLoaded(0, 1);
    });

    expect(result.current.cache.get(slab11)?.path).toBe('/tmp/file-11');
    expect(result.current.cache.get(slab22)?.path).toBe('/tmp/file-22');
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
  });

  it('resets cache when results version changes', async () => {
    const first = [33 as SlabIndex, 44 as SlabIndex];
    const { result, rerender } = renderDataLoader({ results: first, version: 1 });

    await act(async () => {
      await result.current.ensureRangeLoaded(0, 1);
    });

    await waitFor(() => expect(result.current.cache.size).toBe(2));
    expect(mockedInvoke).toHaveBeenCalledTimes(1);

    rerender({ results: first, version: 2 });

    await waitFor(() => expect(result.current.cache.size).toBe(0));

    await act(async () => {
      await result.current.ensureRangeLoaded(0, 1);
    });

    await waitFor(() => expect(result.current.cache.size).toBe(2));
    expect(mockedInvoke).toHaveBeenCalledTimes(2);
  });

  it('ignores stale node info responses after the results version changes', async () => {
    const slab11 = 11 as SlabIndex;
    const slab22 = 22 as SlabIndex;
    const deferred = createDeferred<BuiltNodeInfo[]>();
    let getNodesInfoCalls = 0;

    mockedInvoke.mockImplementation((command: string, payload?: unknown) => {
      if (command !== 'get_nodes_info') {
        return Promise.resolve(null);
      }

      getNodesInfoCalls += 1;
      const slabIndices = (payload as { results: SlabIndex[] }).results;
      if (getNodesInfoCalls === 1) {
        return deferred.promise;
      }
      return Promise.resolve(slabIndices.map((slabIndex) => buildNodeInfo(slabIndex)));
    });

    const { result, rerender } = renderDataLoader({ results: [slab11], version: 1 });

    act(() => {
      void result.current.ensureRangeLoaded(0, 0);
    });

    rerender({ results: [slab22], version: 2 });

    await act(async () => {
      await result.current.ensureRangeLoaded(0, 0);
    });

    await waitFor(() => {
      expect(result.current.cache.get(slab22)?.path).toBe('/tmp/file-22');
    });

    await act(async () => {
      deferred.resolve([buildNodeInfo(slab11)]);
      await deferred.promise;
    });

    expect(result.current.cache.get(slab11)).toBeUndefined();
    expect(result.current.cache.get(slab22)?.path).toBe('/tmp/file-22');
  });

  it('cleans up icon update subscription on unmount', async () => {
    const { unmount } = renderDataLoader({ results: [11 as SlabIndex], version: 1 });
    unmount();

    expect(iconUpdateUnlisten).toHaveBeenCalled();
  });

  it('preserves painted rows and fetches only affected cached paths without extracting icons again', async () => {
    const ids = [11, 22] as SlabIndex[];
    const { result, rerender } = renderHook(
      ({ version, path }) =>
        useDataLoader(ids, version, 1, [{ path, flagBits: 4096, eventId: version, timestamp: 0 }]),
      { initialProps: { version: 1, path: '/unrelated' } },
    );
    await act(async () => {
      await result.current.ensureRangeLoaded(0, 1);
    });
    const cache = result.current.cache;
    rerender({ version: 2, path: '/unrelated' });
    await act(async () => {
      await result.current.ensureRangeLoaded(0, 1);
    });
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
    expect(result.current.cache).toBe(cache);
    const pending = createDeferred<BuiltNodeInfo[]>();
    mockedInvoke.mockReturnValueOnce(pending.promise);
    rerender({ version: 3, path: '/tmp/file-11' });
    let loading!: Promise<void>;
    act(() => {
      loading = result.current.ensureRangeLoaded(0, 1);
    });
    expect(result.current.cache).toBe(cache);
    expect(mockedInvoke).toHaveBeenLastCalledWith('get_nodes_info', {
      results: [11],
      includeIcons: false,
    });
    await act(async () => {
      pending.resolve([buildNodeInfo(ids[0])]);
      await loading;
    });
    expect(result.current.cache).toBe(cache);
  });

  it('does not release a newer batch when an older request for the same ID completes', async () => {
    const ids = [11] as SlabIndex[];
    const first = createDeferred<BuiltNodeInfo[]>();
    const second = createDeferred<BuiltNodeInfo[]>();
    mockedInvoke.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const { result, rerender } = renderDataLoader({ results: ids, version: 1 });
    act(() => {
      void result.current.ensureRangeLoaded(0, 0);
    });
    rerender({ results: ids, version: 2 });
    act(() => {
      void result.current.ensureRangeLoaded(0, 0);
    });
    await act(async () => {
      first.resolve([buildNodeInfo(ids[0])]);
      await first.promise;
    });
    await act(async () => {
      await result.current.ensureRangeLoaded(0, 0);
    });
    expect(mockedInvoke).toHaveBeenCalledTimes(2);
    await act(async () => {
      second.resolve([buildNodeInfo(ids[0])]);
      await second.promise;
    });
  });
  it('finishes a slow row read across unrelated indexed revisions', async () => {
    const ids = [11, 22] as SlabIndex[];
    const pending = createDeferred<BuiltNodeInfo[]>();
    mockedInvoke.mockReturnValueOnce(pending.promise);
    const { result, rerender } = renderHook(
      ({ version }) => useDataLoader(ids, version, 1, [], [22]),
      { initialProps: { version: 1 } },
    );
    let request!: Promise<void>;
    act(() => {
      request = result.current.ensureRangeLoaded(0, 0);
    });
    rerender({ version: 2 });
    await act(async () => {
      await result.current.ensureRangeLoaded(0, 0);
    });
    await act(async () => {
      pending.resolve([buildNodeInfo(ids[0])]);
      await request;
    });
    expect(result.current.cache.get(ids[0])?.path).toBe('/tmp/file-11');
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
  });
});
