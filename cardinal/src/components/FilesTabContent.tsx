import type { ResultPages } from '../utils/resultPages';
import React from 'react';
import type { CSSProperties, ReactNode } from 'react';
import type { MouseEvent as ReactMouseEvent } from 'react';
import { ColumnHeader } from './ColumnHeader';
import { StateDisplay, type DisplayState } from './StateDisplay';
import { VirtualList } from './VirtualList';
import type { ColumnKey } from '../constants';
import type { VirtualListHandle } from './VirtualList';
import type { SearchResultItem } from '../types/search';
import type { SlabIndex } from '../types/slab';
import type { SortKey, SortState } from '../types/sort';
import type { FileChangeBatch } from '../types/ipc';

type FilesTabContentProps = {
  headerRef: React.Ref<HTMLDivElement>;
  onResizeStart: (columnKey: ColumnKey) => (event: ReactMouseEvent<HTMLSpanElement>) => void;
  onHeaderContextMenu: (event: ReactMouseEvent<HTMLDivElement>) => void;
  displayState: DisplayState;
  searchErrorMessage: string | null;
  currentQuery: string;
  currentDirectoryQuery: string;
  virtualListRef: React.Ref<VirtualListHandle>;
  results: SlabIndex[];
  pages?: ResultPages;
  // Committed data epoch. queryVersion resets the cache; live changes revalidate paths.
  dataResultsVersion: number;
  // Bumps when visible ordering changes; VirtualList uses it for viewport-dependent refresh work.
  displayedResultsVersion: number;
  queryVersion?: number;
  fileChanges?: FileChangeBatch;
  changedIds?: readonly number[];
  rowHeight: number;
  overscan: number;
  renderRow: (
    rowIndex: number,
    item: SearchResultItem | undefined,
    rowStyle: CSSProperties,
  ) => ReactNode;
  onScrollSync: (scrollLeft: number) => void;
  sortState: SortState;
  isSorting?: boolean;
  onSortToggle: (sortKey: SortKey) => void;
  sortDisabled: boolean;
  sortDisabledTooltip: string | null;
};

export function FilesTabContent({
  headerRef,
  onResizeStart,
  onHeaderContextMenu,
  displayState,
  searchErrorMessage,
  currentQuery,
  currentDirectoryQuery,
  virtualListRef,
  results,
  pages,
  dataResultsVersion,
  displayedResultsVersion,
  queryVersion,
  fileChanges,
  changedIds,
  rowHeight,
  overscan,
  renderRow,
  onScrollSync,
  sortState,
  isSorting,
  onSortToggle,
  sortDisabled,
  sortDisabledTooltip,
}: FilesTabContentProps): React.JSX.Element {
  return (
    <div className="scroll-area">
      <ColumnHeader
        ref={headerRef}
        onResizeStart={onResizeStart}
        onContextMenu={onHeaderContextMenu}
        isSorting={isSorting}
        sortState={sortState}
        onSortToggle={onSortToggle}
        sortDisabled={sortDisabled}
        sortDisabledTooltip={sortDisabledTooltip}
      />
      <div className="flex-fill">
        {displayState !== 'results' ? (
          <StateDisplay
            state={displayState}
            message={searchErrorMessage}
            query={currentQuery}
            directoryQuery={currentDirectoryQuery}
          />
        ) : (
          <VirtualList
            ref={virtualListRef}
            results={results}
            pages={pages}
            dataResultsVersion={dataResultsVersion}
            displayedResultsVersion={displayedResultsVersion}
            queryVersion={queryVersion}
            fileChanges={fileChanges}
            changedIds={changedIds}
            rowHeight={rowHeight}
            overscan={overscan}
            renderRow={renderRow}
            onScrollSync={onScrollSync}
          />
        )}
      </div>
    </div>
  );
}
