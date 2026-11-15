import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { ComponentProps } from 'react';
import { invoke } from '@tauri-apps/api/core';
import Scrollbar from './Scrollbar';
import { useTranslation } from 'react-i18next';
import { useContextMenu } from '../hooks/useContextMenu';
import type { VolumeExclusion } from '../utils/pathExclusions';
import { FileRow } from './FileRow';
import { ColumnHeader } from './ColumnHeader';
import { StateDisplay } from './StateDisplay';
import { ROW_HEIGHT } from '../constants';
import type { VolumeInfo } from '../hooks/useVolumes';
import type { SearchResultItem } from '../types/search';
import type { SortState } from '../types/sort';

type Page = { total: number; indexed: number; rows: SearchResultItem[]; highlights: string[] };
const PAGE = 256;
let serial = 0;
const nextRequest = () => (serial = Math.max(Date.now() * 1000, serial + 1));
type Props = {
  volume: VolumeInfo;
  exclusions: VolumeExclusion[];
  onExclude: (path: string, volume: VolumeInfo) => void;
  query: string;
  directory: string;
  caseSensitive: boolean;
  onResizeStart: ComponentProps<typeof ColumnHeader>['onResizeStart'];
  onCount: (n: { matched: number; indexed: number } | null) => void;
};

export default function VolumeFiles({
  volume,
  exclusions,
  onExclude,
  query,
  directory,
  caseSensitive,
  onResizeStart,
  onCount,
}: Props) {
  const { t } = useTranslation();
  const [sort, setSort] = useState<SortState>(null);
  const [scroll, setScroll] = useState(0);
  const [height, setHeight] = useState(0);
  const [data, setData] = useState<{ offset: number; page: Page }>();
  const [error, setError] = useState<string>();
  const [selected, setSelected] = useState<string>();
  const [pending, setPending] = useState(true);
  const viewport = useRef<HTMLDivElement>(null);
  const header = useRef<HTMLDivElement>(null);
  const refresh = useRef<() => void>();
  useLayoutEffect(() => {
    const element = viewport.current!;
    const measure = () => setHeight(element.clientHeight);
    const observer = new ResizeObserver(measure);
    measure();
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const visible = data?.page.total ?? 0;
  const maxScroll = Math.max(0, visible * ROW_HEIGHT - height);
  const top = Math.min(scroll, maxScroll);
  const start = Math.max(0, Math.floor(top / ROW_HEIGHT) - 4);
  const end = Math.min(visible, Math.ceil((top + height) / ROW_HEIGHT) + 4);
  const offset = Math.floor(start / PAGE) * PAGE;
  const limit = Math.min(1024, Math.max(PAGE, Math.ceil((end - offset) / PAGE) * PAGE));
  const scrollBy = useCallback(
    (update: (n: number) => number) =>
      setScroll((n) => Math.min(maxScroll, Math.max(0, update(n)))),
    [maxScroll],
  );
  const request = useMemo(
    () => ({
      query: query.trim() || null,
      directoryQuery: directory.trim() || null,
      caseSensitive,
      sort,
      excludedPaths: exclusions
        .filter((rule) => rule.volumeId === volume.id)
        .map((rule) => rule.relativePath),
      requestId: nextRequest(),
    }),
    [query, directory, caseSensitive, sort, volume.id, exclusions],
  );
  useEffect(() => {
    setScroll(0);
    setSelected(undefined);
  }, [request]);
  useEffect(() => {
    let alive = true;
    let running = false;
    let dirty = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setPending(true);
    const schedule = () => {
      dirty = true;
      if (running || timer !== undefined || !alive) return;
      timer = setTimeout(async () => {
        timer = undefined;
        dirty = false;
        running = true;
        try {
          const page = await invoke<Page>('query_volume', {
            id: volume.id,
            request: { ...request, offset, limit },
          });
          if (!alive) return;
          setData({ page, offset });
          setError(undefined);
          onCount({ matched: page.total, indexed: page.indexed });
          if (offset >= page.total && offset !== 0) setScroll(0);
        } catch (reason) {
          if (alive) {
            setError(String(reason));
            onCount(null);
          }
        } finally {
          running = false;
          if (alive) {
            setPending(false);
            if (dirty) schedule();
          }
        }
      }, 24);
    };
    refresh.current = schedule;
    schedule();
    return () => {
      alive = false;
      clearTimeout(timer);
      refresh.current = undefined;
    };
  }, [volume.id, request, offset, limit, onCount]);
  useEffect(() => {
    refresh.current?.();
  }, [volume.revision]);
  const open = useCallback(
    (path: string, reveal = false) => {
      if (volume.online)
        void invoke('open_volume_path', { id: volume.id, path, reveal }).catch((e) =>
          setError(String(e)),
        );
    },
    [volume.id, volume.online],
  );
  const { showContextMenu } = useContextMenu(null, undefined, { online: volume.online, open });
  return (
    <div
      className="scroll-area volume-files"
      tabIndex={0}
      onKeyDown={(e) => {
        if ((e.key === 'ArrowDown' || e.key === 'ArrowUp') && data && visible) {
          e.preventDefault();
          const i = data.page.rows.findIndex((r) => r.path === selected);
          const next = Math.max(
            0,
            Math.min(
              visible - 1,
              i < 0
                ? Math.floor(top / ROW_HEIGHT)
                : data.offset + i + (e.key === 'ArrowDown' ? 1 : -1),
            ),
          );
          setSelected(data.page.rows[next - data.offset]?.path);
          if (next * ROW_HEIGHT < top) setScroll(next * ROW_HEIGHT);
          if ((next + 1) * ROW_HEIGHT > top + height) setScroll((next + 1) * ROW_HEIGHT - height);
        }
        if (!selected) return;
        if ((e.key === 'Enter' || (e.metaKey && e.key === 'o')) && volume.online) {
          e.preventDefault();
          open(selected);
        }
        if (e.metaKey && e.key === 'r' && volume.online) {
          e.preventDefault();
          open(selected, true);
        }
        if (e.metaKey && e.key === 'c') {
          e.preventDefault();
          void navigator.clipboard?.writeText(selected);
        }
      }}
    >
      <ColumnHeader
        ref={header}
        onResizeStart={onResizeStart}
        onContextMenu={(e) => e.preventDefault()}
        sortState={sort}
        isSorting={pending}
        onSortToggle={(key) =>
          setSort((s) => ({
            key,
            direction: s?.key === key && s.direction === 'asc' ? 'desc' : 'asc',
          }))
        }
        sortDisabled={false}
        sortDisabledTooltip={null}
      />
      {volume.error && (
        <div role="status" className="volume-message">
          {volume.error}
        </div>
      )}
      <div className="flex-fill" ref={viewport}>
        {error ? (
          <StateDisplay state="error" message={error} />
        ) : visible > 0 ? (
          <div
            className="virtual-list"
            role="list"
            aria-rowcount={visible}
            onWheel={(e) => {
              const scale = e.deltaMode === 1 ? ROW_HEIGHT : e.deltaMode === 2 ? height : 1;
              scrollBy((n) => n + e.deltaY * scale);
            }}
          >
            <div
              className="virtual-list-viewport"
              onScroll={(e) => {
                if (header.current) header.current.scrollLeft = e.currentTarget.scrollLeft;
              }}
            >
              <div className="virtual-list-items">
                {Array.from({ length: end - start }, (_, i) => {
                  const index = start + i;
                  const item = data?.page.rows[index - data.offset];
                  return (
                    item && (
                      <FileRow
                        key={item.path}
                        rowIndex={index}
                        item={item}
                        style={{
                          position: 'absolute',
                          top: index * ROW_HEIGHT - top,
                          height: ROW_HEIGHT,
                          width: 'var(--columns-total)',
                        }}
                        highlightTerms={data?.page.highlights}
                        caseInsensitive={!caseSensitive}
                        isSelected={selected === item.path}
                        onSelect={() => setSelected(item.path)}
                        onOpen={volume.online ? open : undefined}
                        onContextMenu={(event, path, _index, isDirectory) => {
                          setSelected(path);
                          showContextMenu(event, [path], {
                            path,
                            isDirectory,
                            root: volume.path,
                            exclude: (p) => onExclude(p, volume),
                          });
                        }}
                        allowDrag={false}
                      />
                    )
                  );
                })}
              </div>
            </div>
            <Scrollbar
              totalHeight={visible * ROW_HEIGHT}
              viewportHeight={height}
              maxScrollTop={maxScroll}
              scrollTop={top}
              onScrollUpdate={scrollBy}
            />
          </div>
        ) : !data || volume.indexing ? (
          <StateDisplay state="loading" />
        ) : (
          <StateDisplay state="empty" query={query} directoryQuery={directory} />
        )}
      </div>
      {!volume.online && <div className="volume-message">{t('volumes.offlineHint')}</div>}
    </div>
  );
}
