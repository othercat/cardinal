import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { subscribeIndexedEvents } from '../../runtime/tauriEventRuntime';
import { LIVE_SEARCH_REFRESH_MS, useLiveSearchRefresh } from '../useLiveSearchRefresh';

vi.mock('../../runtime/tauriEventRuntime', () => ({ subscribeIndexedEvents: vi.fn() }));
const subscribe = vi.mocked(subscribeIndexedEvents);
const event = { path: '/changing.txt', flagBits: 4096, eventId: 1, timestamp: 0 };

describe('useLiveSearchRefresh', () => {
  let emit: Parameters<typeof subscribeIndexedEvents>[0];
  const unlisten = vi.fn();
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
    subscribe.mockImplementation((callback) => {
      emit = callback;
      return unlisten;
    });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('coalesces bursts and refreshes during a continuous stream', async () => {
    const refresh = vi.fn().mockResolvedValue(undefined);
    renderHook(() => useLiveSearchRefresh(refresh, true));
    await act(async () => {
      for (let i = 0; i < 35; i++) {
        emit([event]);
        await vi.advanceTimersByTimeAsync(3);
      }
    });
    expect(refresh.mock.calls.length).toBeGreaterThanOrEqual(3);
    expect(refresh.mock.calls.length).toBeLessThanOrEqual(4);
  });

  it('does no hidden-window work and refreshes when visible again', async () => {
    const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
    const refresh = vi.fn().mockResolvedValue(undefined);
    renderHook(() => useLiveSearchRefresh(refresh, true));
    await act(async () => {
      emit([event]);
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(refresh).not.toHaveBeenCalled();
    await act(async () => {
      hidden.mockReturnValue(false);
      document.dispatchEvent(new Event('visibilitychange'));
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it('keeps one request in flight and catches up on changes during it', async () => {
    let finish: () => void = () => {};
    const refresh = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise<void>((resolve) => {
            finish = resolve;
          }),
      )
      .mockResolvedValue(undefined);
    renderHook(() => useLiveSearchRefresh(refresh, true));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LIVE_SEARCH_REFRESH_MS);
    });
    await act(async () => {
      emit([event]);
      emit([event]);
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(refresh).toHaveBeenCalledTimes(1);
    await act(async () => {
      finish();
      await vi.advanceTimersByTimeAsync(LIVE_SEARCH_REFRESH_MS);
    });
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it('waits for a ready index and cleans up timers and subscriptions', async () => {
    const refresh = vi.fn().mockResolvedValue(undefined);
    const { rerender, unmount } = renderHook(({ ready }) => useLiveSearchRefresh(refresh, ready), {
      initialProps: { ready: false },
    });
    expect(subscribe).not.toHaveBeenCalled();
    rerender({ ready: true });
    expect(subscribe).toHaveBeenCalledTimes(1);
    unmount();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(refresh).not.toHaveBeenCalled();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('retains changes deferred by user activity and does no idle polling', async () => {
    const refresh = vi.fn().mockResolvedValueOnce(false).mockResolvedValue(true);
    renderHook(() => useLiveSearchRefresh(refresh, true));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LIVE_SEARCH_REFRESH_MS + 100);
    });
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(refresh).toHaveBeenLastCalledWith(null);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10000);
    });
    expect(refresh).toHaveBeenCalledTimes(2);
    await act(async () => {
      emit([event, { ...event, flagBits: 512 }]);
      await vi.advanceTimersByTimeAsync(LIVE_SEARCH_REFRESH_MS);
    });
    expect(refresh).toHaveBeenLastCalledWith([{ ...event, flagBits: 4096 | 512 }]);
  });
});
