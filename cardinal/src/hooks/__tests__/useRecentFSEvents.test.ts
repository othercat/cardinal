import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { subscribeFSEventsBatch } from '../../runtime/tauriEventRuntime';
import { useRecentFSEvents } from '../useRecentFSEvents';
import { eventPathFilter } from '../../utils/pathExclusions';

vi.mock('../../runtime/tauriEventRuntime', () => ({
  subscribeFSEventsBatch: vi.fn(),
}));

const mockedSubscribeFSEventsBatch = vi.mocked(subscribeFSEventsBatch);

describe('useRecentFSEvents', () => {
  const unlisten = vi.fn();
  let fsEventsBatchListener: ((payload: any) => void) | null = null;

  beforeEach(() => {
    vi.clearAllMocks();
    fsEventsBatchListener = null;

    mockedSubscribeFSEventsBatch.mockImplementation((callback: (payload: any) => void) => {
      fsEventsBatchListener = callback;
      return unlisten;
    });
  });

  it('stores and filters streamed events when active', async () => {
    const { result, rerender } = renderHook((props) => useRecentFSEvents(props), {
      initialProps: {
        caseSensitive: false,
        isActive: true,
        eventFilterQuery: '',
      },
    });

    await waitFor(() => {
      expect(fsEventsBatchListener).not.toBeNull();
    });

    act(() => {
      fsEventsBatchListener?.([
        { path: '/tmp/Alpha.txt', eventId: 1, timestamp: 1, flagBits: 0 },
        { path: '/tmp/beta.txt', eventId: 2, timestamp: 2, flagBits: 0 },
      ]);
    });

    expect(result.current.filteredEvents).toHaveLength(2);

    rerender({
      caseSensitive: false,
      isActive: true,
      eventFilterQuery: 'alpha',
    });

    expect(result.current.filteredEvents).toHaveLength(1);
    expect(result.current.filteredEvents[0]?.path).toBe('/tmp/Alpha.txt');
  });

  it('cleans up runtime subscription on unmount', async () => {
    const { unmount } = renderHook(() =>
      useRecentFSEvents({ caseSensitive: false, isActive: true, eventFilterQuery: '' }),
    );
    unmount();

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('updates existing event rows for exclusions, explicit includes, and undo', () => {
    const { result, rerender } = renderHook(
      ({ ignored }: { ignored: string[] }) =>
        useRecentFSEvents({
          caseSensitive: true,
          isActive: true,
          eventFilterQuery: '',
          showPath: eventPathFilter(ignored, ['~/src/cache/keep'], '/Users/test'),
        }),
      { initialProps: { ignored: [] as string[] } },
    );
    const paths = ['cache/other/a', 'cache', 'cache/keep/b', 'cache2/a'].map(
      (s) => '/Users/test/src/' + s,
    );
    act(() =>
      fsEventsBatchListener?.(
        paths.map((path, eventId) => ({ path, eventId, timestamp: 1, flagBits: 0 })),
      ),
    );
    rerender({ ignored: ['~/src/cache'] });
    expect(result.current.filteredEvents.map((e) => e.path)).toEqual(paths.slice(1));
    rerender({ ignored: [] });
    expect(result.current.filteredEvents.map((e) => e.path)).toEqual(paths);
  });

  it('keeps the last 10,000 raw notifications in order while the Files tab is open', () => {
    const { result, rerender } = renderHook(
      ({ active }) =>
        useRecentFSEvents({ isActive: active, caseSensitive: false, eventFilterQuery: '' }),
      { initialProps: { active: false } },
    );
    const rows = Array.from({ length: 10_019 }, (_, eventId) => ({
      path: '/scratch/hot.tmp',
      eventId,
      timestamp: 1,
      flagBits: 4096,
    }));
    act(() => fsEventsBatchListener?.(rows));
    expect(result.current.filteredEvents).toHaveLength(0);
    rerender({ active: true });
    expect(result.current.filteredEvents.map((e) => e.eventId)).toEqual(
      rows.slice(19).map((e) => e.eventId),
    );
  });
});
