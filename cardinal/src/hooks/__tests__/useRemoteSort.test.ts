import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import type { SlabIndex } from '../../types/slab';
import { useRemoteSort } from '../useRemoteSort';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

const mockedInvoke = vi.mocked(invoke);

const toSlabIndices = (values: number[]): SlabIndex[] => values.map((value) => value as SlabIndex);

const createDeferred = () => {
  let resolve: (value: SlabIndex[]) => void = () => {};
  const promise = new Promise<SlabIndex[]>((res) => {
    resolve = res;
  });
  return { promise, resolve };
};

describe('useRemoteSort', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.localStorage.clear();

    mockedInvoke.mockImplementation((command: string) => {
      if (command === 'get_sorted_view') {
        return Promise.resolve(toSlabIndices([2, 1, 0]));
      }
      return Promise.resolve(null);
    });
  });

  it('bumps displayedResultsVersion when sort projection changes without backend version changes', async () => {
    const results = toSlabIndices([0, 1, 2]);
    const { result } = renderHook(() => useRemoteSort(results, 1, 'en-US', () => null));

    await waitFor(() => {
      expect(result.current.displayedResultsVersion).toBeGreaterThan(0);
    });
    const beforeSortVersion = result.current.displayedResultsVersion;

    act(() => {
      result.current.handleSortToggle('filename');
    });

    await waitFor(() => {
      expect(mockedInvoke).toHaveBeenCalledWith('get_sorted_view', {
        results,
        sort: { key: 'filename', direction: 'asc' },
        requestId: expect.any(Number),
      });
    });
    await waitFor(() => {
      expect(result.current.displayedResultsVersion).toBeGreaterThan(beforeSortVersion);
    });
  });

  it('bumps displayedResultsVersion when backend resultsVersion increments', async () => {
    const first = toSlabIndices([0, 1, 2]);
    const next = toSlabIndices([10, 11, 12]);

    const { result, rerender } = renderHook(
      ({ items, version }: { items: SlabIndex[]; version: number }) =>
        useRemoteSort(items, version, 'en-US', () => null),
      {
        initialProps: {
          items: first,
          version: 1,
        },
      },
    );

    await waitFor(() => {
      expect(result.current.displayedResultsVersion).toBeGreaterThan(0);
    });
    const beforeRefreshVersion = result.current.displayedResultsVersion;

    act(() => {
      rerender({
        items: next,
        version: 2,
      });
    });

    await waitFor(() => {
      expect(result.current.displayedResultsVersion).toBeGreaterThan(beforeRefreshVersion);
    });
    expect(result.current.displayedResults).toEqual(next);
  });

  it('does not sort remotely when the result count exceeds threshold', async () => {
    window.localStorage.setItem('cardinal.sortThreshold', '2');
    const results = toSlabIndices([0, 1, 2]);
    const { result } = renderHook(() => useRemoteSort(results, 1, 'en-US', () => null));

    act(() => {
      result.current.handleSortToggle('filename');
    });

    await waitFor(() => {
      expect(result.current.sortState).toBeNull();
    });
    expect(mockedInvoke).not.toHaveBeenCalledWith(
      'get_sorted_view',
      expect.objectContaining({ results }),
    );
    expect(result.current.displayedResults).toEqual(results);
  });

  it('sorts more than 20000 results and migrates the old default limit', async () => {
    window.localStorage.setItem('cardinal.sortThreshold', '20000');
    const results = toSlabIndices(Array.from({ length: 25001 }, (_, i) => i));
    mockedInvoke.mockResolvedValue([...results].reverse());
    const { result } = renderHook(() => useRemoteSort(results, 1, 'en-US', () => null));
    act(() => result.current.handleSortToggle('filename'));
    await waitFor(() => expect(result.current.displayedResults[0]).toBe(25000));
    expect(result.current.displayedResults).toHaveLength(results.length);
    expect(result.current.sortThreshold).toBe(0);
    expect(result.current.canSort).toBe(true);
  });

  it('retains an explicitly chosen 20000 limit after remounting', () => {
    const results = toSlabIndices([1]);
    const first = renderHook(() => useRemoteSort(results, 1, 'en-US', () => null));
    act(() => first.result.current.setSortThreshold(20000));
    first.unmount();
    const second = renderHook(() => useRemoteSort(results, 1, 'en-US', () => null));
    expect(second.result.current.sortThreshold).toBe(20000);
  });

  it('shows current results rather than a previous sorted view while a refresh is pending', async () => {
    const initial = toSlabIndices([0, 1, 2]);
    const next = toSlabIndices([10, 11, 12]);
    const pending = createDeferred();
    const { result, rerender } = renderHook(
      ({ items, version }) => useRemoteSort(items, version, 'en-US', () => null),
      { initialProps: { items: initial, version: 1 } },
    );
    act(() => result.current.handleSortToggle('size'));
    await waitFor(() => expect(result.current.displayedResults[0]).toBe(2));
    mockedInvoke.mockReturnValueOnce(pending.promise);
    rerender({ items: next, version: 2 });
    expect(result.current.displayedResults).toBe(next);
    await act(async () => pending.resolve([...next].reverse()));
    expect(result.current.displayedResults).toEqual([...next].reverse());
  });

  it('settles failed sorting without an unhandled rejection or stale rows', async () => {
    const log = vi.spyOn(console, 'error').mockImplementation(() => {});
    mockedInvoke.mockRejectedValueOnce(new Error('backend unavailable'));
    const results = toSlabIndices([1, 2]);
    const { result } = renderHook(() => useRemoteSort(results, 1, 'en-US', () => null));
    act(() => result.current.handleSortToggle('filename'));
    await waitFor(() => expect(log).toHaveBeenCalled());
    expect(result.current.isSorting).toBe(false);
    expect(result.current.displayedResults).toBe(results);
    log.mockRestore();
  });

  it('keeps the completed order and viewport version while silently re-sorting live data', async () => {
    const initial = toSlabIndices([0, 1, 2]);
    const pending = createDeferred();
    const { result, rerender } = renderHook(
      ({ items, version }) =>
        useRemoteSort(items, version, 'en-US', () => null, { queryVersion: 1, fileChanges: null }),
      { initialProps: { items: initial, version: 1 } },
    );
    act(() => result.current.handleSortToggle('size'));
    await waitFor(() => expect(result.current.displayedResults).toEqual([2, 1, 0]));
    const order = result.current.displayedResults;
    const version = result.current.displayedResultsVersion;
    mockedInvoke.mockReturnValueOnce(pending.promise);
    rerender({ items: [...initial], version: 2 });
    expect(result.current.displayedResults).toBe(order);
    expect(result.current.displayedResultsVersion).toBe(version);
    expect(result.current.displayedDataVersion).toBe(1);
    expect(result.current.sortButtonsDisabled).toBe(false);
    await act(async () => {
      pending.resolve([2, 1, 0] as SlabIndex[]);
    });
    expect(result.current.displayedResults).toBe(order);
    expect(result.current.displayedResultsVersion).toBe(version);
    expect(result.current.displayedDataVersion).toBe(2);
  });

  it('preserves current sort state when result count stays within threshold (including empty results)', async () => {
    const initial = toSlabIndices([0, 1, 2]);

    const { result, rerender } = renderHook(
      ({ items, version }: { items: SlabIndex[]; version: number }) =>
        useRemoteSort(items, version, 'en-US', () => null),
      {
        initialProps: {
          items: initial,
          version: 1,
        },
      },
    );

    act(() => {
      result.current.handleSortToggle('filename');
    });

    await waitFor(() => {
      expect(result.current.sortState).toEqual({ key: 'filename', direction: 'asc' });
    });

    act(() => {
      rerender({
        items: [],
        version: 2,
      });
    });

    await waitFor(() => {
      expect(result.current.sortState).toEqual({ key: 'filename', direction: 'asc' });
    });
    expect(result.current.displayedResults).toEqual([]);
  });

  it('allows toggling sort state for empty results without remote sorting', async () => {
    const empty: SlabIndex[] = [];
    const { result } = renderHook(() => useRemoteSort(empty, 1, 'en-US', () => null));

    expect(result.current.sortButtonsDisabled).toBe(false);

    act(() => {
      result.current.handleSortToggle('filename');
    });

    await waitFor(() => {
      expect(result.current.sortState).toEqual({ key: 'filename', direction: 'asc' });
    });
    expect(result.current.displayedResults).toEqual(empty);
    expect(mockedInvoke).not.toHaveBeenCalled();
  });

  it('preserves current sort state when an optional limit is exceeded', async () => {
    window.localStorage.setItem('cardinal.sortThreshold', '2');
    const initial = toSlabIndices([0, 1]);
    const overLimit = toSlabIndices([0, 1, 2]);

    const { result, rerender } = renderHook(
      ({ items, version }: { items: SlabIndex[]; version: number }) =>
        useRemoteSort(items, version, 'en-US', () => null),
      {
        initialProps: {
          items: initial,
          version: 1,
        },
      },
    );

    act(() => {
      result.current.handleSortToggle('filename');
    });

    await waitFor(() => {
      expect(result.current.sortState).toEqual({ key: 'filename', direction: 'asc' });
    });

    act(() => {
      rerender({
        items: overLimit,
        version: 2,
      });
    });

    await waitFor(() => {
      expect(result.current.sortState).toEqual({ key: 'filename', direction: 'asc' });
    });
    expect(result.current.displayedResults).toEqual(overLimit);
  });

  it('bumps displayedResultsVersion when switching sorted projection on then off', async () => {
    const results = toSlabIndices([0, 1, 2]);
    const { result } = renderHook(() => useRemoteSort(results, 1, 'en-US', () => null));

    await waitFor(() => {
      expect(result.current.displayedResultsVersion).toBeGreaterThan(0);
    });
    const initialVersion = result.current.displayedResultsVersion;

    act(() => {
      result.current.handleSortToggle('filename');
    });
    await waitFor(() => {
      expect(result.current.sortState).toEqual({ key: 'filename', direction: 'asc' });
    });
    await waitFor(() => {
      expect(result.current.displayedResultsVersion).toBeGreaterThan(initialVersion);
    });
    const sortedVersion = result.current.displayedResultsVersion;

    act(() => {
      result.current.handleSortToggle('filename');
    });
    await waitFor(() => {
      expect(result.current.sortState).toEqual({ key: 'filename', direction: 'desc' });
    });

    act(() => {
      result.current.handleSortToggle('filename');
    });
    await waitFor(() => {
      expect(result.current.sortState).toBeNull();
    });
    await waitFor(() => {
      expect(result.current.displayedResultsVersion).toBeGreaterThan(sortedVersion);
    });
  });

  it('ignores stale remote sort responses and applies only the latest request', async () => {
    const results = toSlabIndices([0, 1, 2]);
    const firstRequest = createDeferred();
    const secondRequest = createDeferred();

    mockedInvoke.mockReset();
    mockedInvoke
      .mockImplementationOnce((command: string) => {
        if (command === 'get_sorted_view') return firstRequest.promise;
        return Promise.resolve(null);
      })
      .mockImplementationOnce((command: string) => {
        if (command === 'get_sorted_view') return secondRequest.promise;
        return Promise.resolve(null);
      });

    const { result } = renderHook(() => useRemoteSort(results, 1, 'en-US', () => null));

    act(() => {
      result.current.handleSortToggle('filename');
    });
    act(() => {
      result.current.handleSortToggle('filename');
    });

    await waitFor(() => {
      expect(mockedInvoke).toHaveBeenCalledTimes(2);
    });
    const firstId = (mockedInvoke.mock.calls[0][1] as { requestId: number }).requestId;
    const secondId = (mockedInvoke.mock.calls[1][1] as { requestId: number }).requestId;
    expect(secondId).toBeGreaterThan(firstId);

    act(() => {
      firstRequest.resolve(toSlabIndices([2, 1, 0]));
    });
    await Promise.resolve();

    act(() => {
      secondRequest.resolve(toSlabIndices([1, 2, 0]));
    });

    await waitFor(() => {
      expect(result.current.displayedResults).toEqual(toSlabIndices([1, 2, 0]));
    });
  });
});
