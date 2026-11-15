import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import App from '../App';
import type { RecentEventPayload } from '../types/ipc';
import type { SortKey } from '../types/sort';

const events = vi.hoisted(() => ({ emit: (_events: RecentEventPayload[]) => {} }));
const mounts = vi.hoisted(() => ({ changed: () => {} }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (_: string, cb: () => void) => {
    mounts.changed = cb;
    return () => {};
  },
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../runtime/tauriEventRuntime', () => ({
  subscribeIndexedEvents: (callback: typeof events.emit) => {
    events.emit = callback;
    return () => {};
  },
  subscribeIconUpdate: () => () => {},
}));
vi.mock('react-i18next', async (importOriginal) => ({
  ...(await importOriginal<typeof import('react-i18next')>()),
  useTranslation: () => ({ t: (key: string) => key, i18n: { language: 'en-US' } }),
}));
vi.mock('../hooks/useFullDiskAccessPermission', () => ({
  useFullDiskAccessPermission: () => ({
    status: 'granted',
    isChecking: false,
    requestPermission: vi.fn(),
  }),
}));
vi.mock('../hooks/useAppPreferences', () => ({
  useAppPreferences: () => ({
    isPreferencesOpen: false,
    closePreferences: vi.fn(),
    trayIconEnabled: false,
    setTrayIconEnabled: vi.fn(),
    watchRoot: '/',
    defaultWatchRoot: '/',
    ignorePaths: [],
    defaultIgnorePaths: [],
    includePaths: [],
    defaultIncludePaths: [],
    home: '/Users/test',
    excludePath: vi.fn(),
    volumeExclusions: [],
    preferencesResetToken: 0,
    handleWatchConfigChange: vi.fn(),
    handleResetPreferences: vi.fn(),
  }),
}));
vi.mock('../hooks/useAppWindowListeners', () => ({ useAppWindowListeners: () => {} }));
vi.mock('../hooks/useAppHotkeys', () => ({ useAppHotkeys: () => {} }));
vi.mock('../hooks/useRecentFSEvents', () => ({
  useRecentFSEvents: () => ({ filteredEvents: [] }),
}));
vi.mock('../hooks/useContextMenu', () => ({
  useContextMenu: () => ({ showContextMenu: vi.fn(), showHeaderContextMenu: vi.fn() }),
}));
vi.mock('../hooks/useQuickLook', () => ({
  useQuickLook: () => ({
    toggleQuickLook: vi.fn(),
    updateQuickLook: vi.fn(),
    closeQuickLook: vi.fn(),
  }),
}));

const mockedInvoke = vi.mocked(invoke);
const tick = async (ms: number) => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
};
const noise = { path: '/unrelated/system.log', flagBits: 4096, eventId: 1, timestamp: 0 };
const paths = ['/z/z.txt', '/a/a.txt', '/m/m.txt'];
let ids: number[];
let backgroundDelay: number;
let sortDelay: number;
let sizes: number[];
const order = (key: SortKey, direction: string) =>
  [...ids].sort((a, b) => {
    const values =
      key === 'size'
        ? sizes
        : key === 'mtime'
          ? [300, 100, 200]
          : key === 'ctime'
            ? [200, 300, 100]
            : paths;
    const av = values[a] ?? a;
    const bv = values[b] ?? b;
    const comparison = av < bv ? -1 : av > bv ? 1 : a - b;
    return direction === 'desc' ? -comparison : comparison;
  });
const visiblePaths = (container: HTMLElement) =>
  Array.from(container.querySelectorAll('.virtual-list-items > [data-row-path]'), (row) =>
    row.getAttribute('data-row-path'),
  );

describe('App live refresh and sorting integration', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    window.localStorage.clear();
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
    Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
      configurable: true,
      get: () => 240,
    });
    ids = [0, 1, 2];
    sizes = [10, 30, 20];
    backgroundDelay = 500;
    sortDelay = 0;
    mockedInvoke.mockImplementation((command, payload) => {
      const args = payload as {
        results?: number[];
        background?: boolean;
        sort?: { key: SortKey; direction: string };
      };
      if (command === 'get_app_status') return Promise.resolve('Ready');
      if (command === 'search') {
        const response = { results: [...ids], statusCode: 0, highlights: [] };
        return args.background
          ? new Promise((resolve) => setTimeout(() => resolve(response), backgroundDelay))
          : Promise.resolve(response);
      }
      if (command === 'get_nodes_info')
        return Promise.resolve(
          args.results!.map((id) => ({
            path: paths[id] ?? `/files/file-${String(id).padStart(3, '0')}.txt`,
            metadata: {
              type: 0,
              size: sizes[id] ?? id,
              mtime: [300, 100, 200][id] ?? id,
              ctime: [200, 300, 100][id] ?? id,
            },
          })),
        );
      if (command === 'get_sorted_view') {
        const result = args.sort ? order(args.sort.key, args.sort.direction) : [];
        return new Promise((resolve) => setTimeout(() => resolve(result), sortDelay));
      }
      return Promise.resolve(null);
    });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('searches a checked drive, keeps its cached rows after eject and restart, and returns to the local query when unchecked', async () => {
    vi.stubGlobal('__TAURI_INTERNALS__', {});
    let online = true;
    const localInvoke = mockedInvoke.getMockImplementation()!;
    const diskPaths = ['/Volumes/Archive/big.txt', '/Volumes/Archive/small.txt'];
    mockedInvoke.mockImplementation((command, payload) => {
      if (command === 'list_volumes')
        return Promise.resolve([
          {
            id: 'volume-a',
            path: '/Volumes/Archive',
            format: 'hfs',
            reusable: true,
            online,
            indexing: false,
            revision: online ? 1 : 2,
            count: 2,
            error: null,
          },
        ]);
      if (command === 'query_volume') {
        const request = (payload as { request: { sort?: { direction: string } } }).request;
        const paths = request.sort?.direction === 'asc' ? [...diskPaths].reverse() : diskPaths;
        return Promise.resolve({ total: 2, rows: paths.map((path) => ({ path })), highlights: [] });
      }
      return localInvoke(command, payload);
    });
    let app = render(<App />);
    await tick(1600);
    fireEvent.click(screen.getByRole('checkbox', { name: /Archive/ }));
    await tick(120);
    const externalRows = () =>
      Array.from(app.container.querySelectorAll('.volume-files [data-row-path]'), (n) =>
        n.getAttribute('data-row-path'),
      );
    expect(externalRows()).toEqual(diskPaths);
    fireEvent.click(screen.getByRole('button', { name: 'columns.size' }));
    await tick(50);
    expect(externalRows()).toEqual([...diskPaths].reverse());
    online = false;
    act(() => mounts.changed());
    await tick(70);
    expect(screen.getByRole('checkbox', { name: /volumes.offline/ })).toBeChecked();
    fireEvent.doubleClick(app.container.querySelector('.volume-files [data-row-path]')!);
    expect(mockedInvoke.mock.calls.some(([cmd]) => cmd === 'open_volume_path')).toBe(false);
    app.unmount();
    app = render(<App />);
    await tick(150);
    expect(screen.getByRole('checkbox', { name: /volumes.offline/ })).toBeChecked();
    expect(externalRows()).toEqual(diskPaths);
    const input = screen.getByRole('textbox', { name: 'search.aria.searchInput' });
    fireEvent.change(input, { target: { value: 'needle' } });
    await tick(400);
    expect(
      mockedInvoke.mock.calls.filter(([cmd]) => cmd === 'query_volume').pop()?.[1],
    ).toMatchObject({ request: { query: 'needle' } });
    fireEvent.click(screen.getByRole('checkbox', { name: /Archive/ }));
    await tick(700);
    expect(externalRows()).toEqual([]);
    expect(visiblePaths(app.container)).toEqual(paths);
    expect(mockedInvoke.mock.calls.filter(([cmd]) => cmd === 'search').pop()?.[1]).toMatchObject({
      query: 'needle',
    });
  });

  it('keeps the real list, scroll position, cached rows and selection through slow unrelated event bursts', async () => {
    ids = Array.from({ length: 100 }, (_, i) => i);
    const { container } = render(<App />);
    await tick(1600); // Initial query plus Ready/history catch-up.
    const list = container.querySelector('.virtual-list')!;
    fireEvent.wheel(list, { deltaY: 480, deltaMode: 0 });
    await tick(50);
    const row = container.querySelectorAll('.virtual-list-items > [data-row-path]')[3];
    const selectedPath = row.getAttribute('data-row-path');
    const before = visiblePaths(container);
    fireEvent.mouseDown(row, { button: 0 });
    expect(row).toHaveAttribute('aria-selected', 'true');
    const nodeCalls = mockedInvoke.mock.calls.filter(([name]) => name === 'get_nodes_info').length;
    const iconCalls = mockedInvoke.mock.calls.filter(
      ([name]) => name === 'update_icon_viewport',
    ).length;
    let removedList = false;
    const observer = new MutationObserver((records) => {
      for (const record of records)
        for (const removed of record.removedNodes) {
          if (removed === list || removed.contains(list)) removedList = true;
        }
    });
    observer.observe(container, { childList: true, subtree: true });
    for (let i = 0; i < 35; i++) {
      act(() => events.emit([{ ...noise, eventId: i + 1 }]));
      await tick(100);
      expect(container.querySelector('.spinner')).toBeNull();
    }
    await tick(2000);
    observer.disconnect();
    expect(removedList).toBe(false);
    expect(container.querySelector('.virtual-list')).toBe(list);
    expect(visiblePaths(container)).toEqual(before);
    expect(container.querySelector('[aria-selected="true"]')).toHaveAttribute(
      'data-row-path',
      selectedPath,
    );
    expect(mockedInvoke.mock.calls.filter(([name]) => name === 'get_nodes_info')).toHaveLength(
      nodeCalls,
    );
    expect(
      mockedInvoke.mock.calls.filter(([name]) => name === 'update_icon_viewport'),
    ).toHaveLength(iconCalls);
  });

  it.each([
    ['filename', 'filename'],
    ['path', 'fullPath'],
    ['size', 'size'],
    ['modified', 'mtime'],
    ['created', 'ctime'],
  ] as const)(
    'completes %s ascending and descending sorts during continuous file events',
    async (label, key) => {
      const { container } = render(<App />);
      await tick(1600);
      sortDelay = 2000;
      const button = screen.getByRole('button', { name: `columns.${label}` });
      fireEvent.click(button);
      expect(visiblePaths(container)).toEqual(order(key, 'asc').map((id) => paths[id]));
      for (let i = 0; i < 21; i++) {
        act(() => events.emit([{ ...noise, eventId: i + 1 }]));
        await tick(100);
        expect(button).not.toBeDisabled();
      }
      expect(visiblePaths(container)).toEqual(order(key, 'asc').map((id) => paths[id]));
      fireEvent.click(button);
      await tick(500);
      expect(visiblePaths(container)).toEqual(order(key, 'desc').map((id) => paths[id]));
      await tick(1600);
      expect(visiblePaths(container)).toEqual(order(key, 'desc').map((id) => paths[id]));
      expect(container.querySelector('.spinner')).toBeNull();
    },
  );

  it('updates real changed metadata and reorders once while keeping an unaffected selected path', async () => {
    const { container } = render(<App />);
    await tick(1600);
    fireEvent.click(screen.getByRole('button', { name: 'columns.size' }));
    await tick(50);
    const row = container.querySelector('[data-row-path="/m/m.txt"]')!;
    fireEvent.mouseDown(row, { button: 0 });
    sizes[0] = 40;
    act(() => events.emit([{ ...noise, path: paths[0] }]));
    await tick(1700);
    expect(visiblePaths(container)).toEqual(['/m/m.txt', '/a/a.txt', '/z/z.txt']);
    expect(container.querySelector('[aria-selected="true"]')).toHaveAttribute(
      'data-row-path',
      '/m/m.txt',
    );
    expect(mockedInvoke).toHaveBeenCalledWith('get_nodes_info', {
      results: [0],
      includeIcons: false,
    });
  });
  it('uses one native view handle and applies a live move to a descending display without requesting another full sort', async () => {
    const fallback = mockedInvoke.getMockImplementation()!;
    let changed = false;
    let revision = 10;
    mockedInvoke.mockImplementation((command, payload) => {
      const args = payload as { baseViewId?: number; viewId?: number; sortKey?: string };
      if (command === 'search') {
        if (args.baseViewId === undefined)
          return Promise.resolve({
            viewId: revision,
            incremental: true,
            statusCode: 0,
            results: ids,
            highlights: [],
          });
        if (!changed)
          return Promise.resolve({
            viewId: revision,
            incremental: true,
            statusCode: 0,
            results: [],
            edits: [],
          });
        changed = false;
        expect(args.sortKey).toBe('size');
        return Promise.resolve({
          viewId: ++revision,
          incremental: true,
          statusCode: 0,
          results: [],
          edits: [],
          changedIds: [0],
          changedPaths: [paths[0]],
          sorted: {
            key: 'size',
            baseViewId: args.baseViewId,
            edits: [
              { at: 0, remove: 1, insert: [] },
              { at: 3, remove: 0, insert: [0] },
            ],
          },
        });
      }
      if (command === 'get_sorted_view') {
        expect(args.viewId).toBe(10);
        expect(args).not.toHaveProperty('results');
        return Promise.resolve(new Uint32Array([0, 2, 1]).buffer);
      }
      return fallback(command, payload);
    });
    const { container } = render(<App />);
    await tick(1600);
    const button = screen.getByRole('button', { name: 'columns.size' });
    fireEvent.click(button);
    await tick(1);
    fireEvent.click(button);
    expect(visiblePaths(container)).toEqual([paths[1], paths[2], paths[0]]);
    changed = true;
    sizes[0] = 40;
    act(() => events.emit([{ ...noise, path: paths[0] }]));
    await tick(40);
    expect(visiblePaths(container)).toEqual([paths[0], paths[1], paths[2]]);
    expect(mockedInvoke.mock.calls.filter(([name]) => name === 'get_sorted_view')).toHaveLength(1);
  });
});
