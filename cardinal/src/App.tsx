import { useRef, useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import VolumeFiles from './components/VolumeFiles';
import { useVolumes } from './hooks/useVolumes';
import type { ChangeEvent, CSSProperties, MouseEvent as ReactMouseEvent } from 'react';
import './App.css';
import { FileRow } from './components/FileRow';
import { SearchBar } from './components/SearchBar';
import { FilesTabContent } from './components/FilesTabContent';
import { PermissionOverlay } from './components/PermissionOverlay';
import PreferencesOverlay from './components/PreferencesOverlay';
import StatusBar from './components/StatusBar';
import type { SearchResultItem } from './types/search';
import { useColumnResize } from './hooks/useColumnResize';
import { useContextMenu } from './hooks/useContextMenu';
import { useFileSearch } from './hooks/useFileSearch';
import { useEventColumnWidths } from './hooks/useEventColumnWidths';
import { useRecentFSEvents } from './hooks/useRecentFSEvents';
import { DEFAULT_SORTABLE_RESULT_THRESHOLD, useRemoteSort } from './hooks/useRemoteSort';
import { useSelection } from './hooks/useSelection';
import { useQuickLook } from './hooks/useQuickLook';
import { ROW_HEIGHT, OVERSCAN_ROW_COUNT } from './constants';
import type { VirtualListHandle } from './components/VirtualList';
import FSEventsPanel from './components/FSEventsPanel';
import type { FSEventsPanelHandle } from './components/FSEventsPanel';
import { useTranslation } from 'react-i18next';
import { useFullDiskAccessPermission } from './hooks/useFullDiskAccessPermission';
import type { DisplayState } from './components/StateDisplay';
import { openResultPath } from './utils/openResultPath';
import { absoluteRule, eventPathFilter } from './utils/pathExclusions';
import { useStableEvent } from './hooks/useStableEvent';
import { useAppHotkeys } from './hooks/useAppHotkeys';
import { useAppPreferences } from './hooks/useAppPreferences';
import { useAppWindowListeners } from './hooks/useAppWindowListeners';
import { useFilesTabEffects } from './hooks/useFilesTabEffects';
import { useFilesTabState } from './hooks/useFilesTabState';

function App() {
  const { volumes, selected: volume, select: selectVolume } = useVolumes();
  const [volumeCount, setVolumeCount] = useState<{ matched: number; indexed: number } | null>(null);
  const {
    state,
    searchParams,
    updateSearchParams,
    queueSearch,
    queueDirectorySearch,
    queueDirectoryScopeOpen,
    handleStatusUpdate,
    setLifecycleState,
    requestRescan,
    pendingSortRef,
    activeSortKey,
  } = useFileSearch(!volume);
  const {
    results,
    resultsVersion,
    queryVersion,
    fileChanges,
    scannedFiles,
    processedEvents,
    rescanErrors,
    currentQuery,
    currentDirectoryQuery,
    highlightTerms,
    showLoadingUI,
    initialFetchCompleted,
    durationMs,
    resultCount,
    searchError,
    lifecycleState,
  } = state;

  const eventsPanelRef = useRef<FSEventsPanelHandle | null>(null);
  const headerRef = useRef<HTMLDivElement | null>(null);
  const virtualListRef = useRef<VirtualListHandle | null>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);

  const { colWidths, onResizeStart, autoFitColumns } = useColumnResize();
  const { caseSensitive, directoryQuery, directoryScopeOpen } = searchParams;
  const { eventColWidths, onEventResizeStart, autoFitEventColumns } = useEventColumnWidths();
  const { t, i18n } = useTranslation();
  // `resultsVersion` tracks raw backend search result-set changes.
  // `displayedResultsVersion` additionally tracks UI ordering/projection changes (e.g. sort toggle).
  const {
    sortState,
    displayedResults,
    displayedPages,
    displayedResultsVersion,
    displayedDataVersion,
    displayedFileChanges,
    displayedChangedIds,
    sortThreshold,
    setSortThreshold,
    sortDisabledTooltip,
    isSorting,
    sortButtonsDisabled,
    handleSortToggle,
  } = useRemoteSort(
    results,
    resultsVersion,
    i18n.language,
    (limit) => t('sorting.disabled', { limit }),
    {
      queryVersion,
      fileChanges,
      pendingSortRef,
      activeSortKey,
      viewId: state.viewId,
      pagedCount: state.pagedCount,
      sortedEdits: state.sortedEdits,
      changedIds: state.changedIds,
      getLoadedWindow: () => virtualListRef.current?.getLoadedWindow?.() ?? null,
    },
  );

  // Centralized selection management for the virtualized files list.
  // Provides memoized helpers for click/keyboard selection and keeps Quick Look hooks fed.
  const {
    selectedIndices,
    selectedIndicesRef,
    activeRowIndex,
    selectedPaths,
    handleRowSelect,
    selectSingleRow,
    clearSelection,
    moveSelection,
  } = useSelection(displayedResults, displayedResultsVersion, virtualListRef, {
    queryVersion,
    dataVersion: displayedDataVersion,
    fileChanges: displayedFileChanges,
    pages: displayedPages,
  });

  const navigateFromSearchToResults = useCallback(() => {
    if (volume) {
      document.querySelector<HTMLElement>('.volume-files')?.focus();
      return;
    }
    if (displayedResults.length === 0) {
      return;
    }

    selectSingleRow(0);
    searchInputRef.current?.blur();
  }, [volume, displayedResults.length, selectSingleRow]);

  const {
    activeTab,
    isSearchFocused,
    handleSearchFocus,
    handleSearchBlur,
    eventFilterQuery,
    setEventFilterQuery,
    onTabChange,
    searchInputValue,
    directoryInputValue,
    onQueryChange,
    onDirectoryQueryChange,
    onDirectoryInputKeyDown,
    onSearchInputKeyDown,
    submitFilesQuery,
  } = useFilesTabState({
    searchQuery: searchParams.query,
    directoryQuery,
    queueSearch,
    queueDirectorySearch,
    onNavigateFromSearchToResults: navigateFromSearchToResults,
  });
  const getQuickLookPaths = useCallback(
    () => (activeTab === 'files' ? selectedPaths : []),
    [activeTab, selectedPaths],
  );
  // Quick Look controller keeps preview panel in sync with whichever rows are currently selected.
  const { toggleQuickLook, updateQuickLook, closeQuickLook } = useQuickLook({
    getPaths: getQuickLookPaths,
  });

  const {
    showContextMenu: showFilesContextMenu,
    showHeaderContextMenu: showFilesHeaderContextMenu,
  } = useContextMenu(autoFitColumns, toggleQuickLook);

  const {
    showContextMenu: showEventsContextMenu,
    showHeaderContextMenu: showEventsHeaderContextMenu,
  } = useContextMenu(autoFitEventColumns);

  const {
    status: fullDiskAccessStatus,
    isChecking: isCheckingFullDiskAccess,
    requestPermission: requestFullDiskAccessPermission,
  } = useFullDiskAccessPermission();

  const focusSearchInput = useCallback(() => {
    requestAnimationFrame(() => {
      const input = searchInputRef.current;
      if (!input) return;
      input.focus();
    });
  }, []);

  const focusAndSelectSearchInput = useCallback(() => {
    requestAnimationFrame(() => {
      const input = searchInputRef.current;
      if (!input) return;
      input.focus();
      input.select();
    });
  }, []);

  useEffect(() => {
    focusAndSelectSearchInput();
  }, [focusAndSelectSearchInput]);

  const refreshSearchResults = useCallback(() => {
    if (!volume) queueSearch(searchParams.query, { immediate: true });
  }, [volume, searchParams.query, queueSearch]);

  const {
    isPreferencesOpen,
    closePreferences,
    trayIconEnabled,
    setTrayIconEnabled,
    watchRoot,
    defaultWatchRoot,
    ignorePaths,
    defaultIgnorePaths,
    includePaths,
    defaultIncludePaths,
    preferencesResetToken,
    handleWatchConfigChange,
    home,
    excludePath,
    volumeExclusions,
    setVolumeExclusions,
    handleResetPreferences,
  } = useAppPreferences({
    fullDiskAccessStatus,
    isCheckingFullDiskAccess,
    refreshSearchResults,
    i18n,
  });
  const exclusionRoot = absoluteRule(watchRoot, home);
  const showEventPath = useMemo(
    () => eventPathFilter(ignorePaths, includePaths, home),
    [ignorePaths, includePaths, home],
  );
  const { filteredEvents } = useRecentFSEvents({
    caseSensitive,
    isActive: activeTab === 'events',
    eventFilterQuery,
    showPath: showEventPath,
  });

  useAppWindowListeners({
    activeTab,
    searchInputRef,
    focusAndSelectSearchInput,
    handleStatusUpdate,
    setLifecycleState,
    submitFilesQuery,
    setEventFilterQuery,
  });

  const navigateSelection = useStableEvent(moveSelection);
  const triggerQuickLook = useStableEvent(toggleQuickLook);

  useAppHotkeys({
    activeTab: volume && activeTab === 'files' ? 'volume' : activeTab,
    activeRowIndex,
    selectedPaths,
    selectedIndicesRef,
    focusSearchInput,
    focusAndSelectSearchInput,
    clearSelection,
    navigateSelection,
    triggerQuickLook,
  });

  useFilesTabEffects({
    activeTab,
    selectedIndices,
    activeRowIndex,
    closeQuickLook,
    updateQuickLook,
    clearSelection,
    resultsVersion: queryVersion,
    virtualListRef,
    eventsPanelRef,
  });

  const onToggleCaseSensitive = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => {
      const nextValue = event.target.checked;
      updateSearchParams({ caseSensitive: nextValue });
    },
    [updateSearchParams],
  );

  const toggleDirectoryScope = useCallback(() => {
    queueDirectoryScopeOpen(!directoryScopeOpen);
  }, [directoryScopeOpen, queueDirectoryScopeOpen]);

  const handleHorizontalSync = useCallback((scrollLeft: number) => {
    // VirtualList drives the scroll position; mirror it onto the sticky header for alignment.
    if (headerRef.current) {
      headerRef.current.scrollLeft = scrollLeft;
    }
  }, []);

  const selectedIndexSet = useMemo(() => new Set(selectedIndices), [selectedIndices]);

  const handleRowContextMenu = useCallback(
    (
      event: ReactMouseEvent<HTMLDivElement>,
      path: string,
      rowIndex: number,
      isDirectory?: boolean,
    ) => {
      const isRowSelected = selectedIndexSet.has(rowIndex);
      if (!isRowSelected) {
        selectSingleRow(rowIndex);
      }
      const targetPaths = isRowSelected && selectedPaths.length > 0 ? selectedPaths : [path];
      showFilesContextMenu(event, targetPaths, {
        path,
        isDirectory,
        root: exclusionRoot,
        exclude: excludePath,
      });
    },
    [
      selectedIndexSet,
      selectedPaths,
      selectSingleRow,
      showFilesContextMenu,
      exclusionRoot,
      excludePath,
    ],
  );

  const handleEventsContextMenu = useCallback(
    (event: ReactMouseEvent<HTMLDivElement>, path: string, isDirectory?: boolean) => {
      showEventsContextMenu(event, [path], {
        path,
        isDirectory,
        root: exclusionRoot,
        exclude: excludePath,
      });
    },
    [showEventsContextMenu, exclusionRoot, excludePath],
  );

  const renderRow = useCallback(
    (rowIndex: number, item: SearchResultItem | undefined, rowStyle: CSSProperties) => {
      if (!item) {
        return (
          <div
            key={`placeholder-${rowIndex}`}
            className="row columns row-loading"
            style={{ ...rowStyle, width: 'var(--columns-total)' }}
          />
        );
      }

      return (
        <FileRow
          key={item.path}
          rowIndex={rowIndex}
          item={item}
          style={{ ...rowStyle, width: 'var(--columns-total)' }}
          isSelected={selectedIndexSet.has(rowIndex)}
          selectedPathsForDrag={selectedPaths}
          caseInsensitive={!caseSensitive}
          highlightTerms={highlightTerms}
          onContextMenu={handleRowContextMenu}
          onSelect={handleRowSelect}
          onOpen={openResultPath}
        />
      );
    },
    [
      handleRowContextMenu,
      handleRowSelect,
      highlightTerms,
      caseSensitive,
      selectedIndexSet,
      selectedPaths,
    ],
  );

  const displayState: DisplayState = (() => {
    if (!initialFetchCompleted) return lifecycleState === 'LoadingIndex' ? 'restoring' : 'loading';
    if (showLoadingUI) return 'loading';
    if (searchError) return 'error';
    if (results.length === 0) return 'empty';
    return 'results';
  })();
  const searchErrorMessage =
    typeof searchError === 'string' ? searchError : (searchError?.message ?? null);

  const containerStyle = useMemo(
    () =>
      ({
        '--w-filename': `${colWidths.filename}px`,
        '--w-path': `${colWidths.path}px`,
        '--w-size': `${colWidths.size}px`,
        '--w-modified': `${colWidths.modified}px`,
        '--w-created': `${colWidths.created}px`,
        '--w-event-flags': `${eventColWidths.event}px`,
        '--w-event-name': `${eventColWidths.name}px`,
        '--w-event-path': `${eventColWidths.path}px`,
        '--w-event-time': `${eventColWidths.time}px`,
        '--columns-events-total': `${
          eventColWidths.event + eventColWidths.name + eventColWidths.path + eventColWidths.time
        }px`,
      }) as CSSProperties,
    [colWidths, eventColWidths],
  );

  const showFullDiskAccessOverlay = fullDiskAccessStatus === 'denied';
  const overlayStatusMessage = isCheckingFullDiskAccess
    ? t('app.fullDiskAccess.status.checking')
    : t('app.fullDiskAccess.status.disabled');
  const caseSensitiveLabel = t('search.options.caseSensitive');
  const directoryScopeLabel = t('search.options.directoryScope');
  const searchPlaceholder =
    activeTab === 'files' ? t('search.placeholder.files') : t('search.placeholder.events');
  const directorySearchPlaceholder = t('search.placeholder.directory');
  const searchAriaLabel = t('search.aria.searchInput');
  const permissionSteps = [
    t('app.fullDiskAccess.steps.one'),
    t('app.fullDiskAccess.steps.two'),
    t('app.fullDiskAccess.steps.three'),
  ];
  const openSettingsLabel = t('app.fullDiskAccess.openSettings');
  const resultsContainerClassName = `results-container${
    isSearchFocused ? ' results-container--search-focused' : ''
  }`;

  return (
    <>
      <main className="container" aria-hidden={showFullDiskAccessOverlay || isPreferencesOpen}>
        <SearchBar
          inputRef={searchInputRef}
          placeholder={searchPlaceholder}
          ariaLabel={searchAriaLabel}
          value={searchInputValue}
          onChange={onQueryChange}
          onKeyDown={onSearchInputKeyDown}
          directoryScopeEnabled={activeTab === 'files'}
          directoryScopeOpen={directoryScopeOpen}
          directoryScopeLabel={directoryScopeLabel}
          directoryPlaceholder={directorySearchPlaceholder}
          directoryValue={directoryInputValue}
          onToggleDirectoryScope={toggleDirectoryScope}
          onDirectoryChange={onDirectoryQueryChange}
          onDirectoryKeyDown={onDirectoryInputKeyDown}
          caseSensitive={caseSensitive}
          onToggleCaseSensitive={onToggleCaseSensitive}
          caseSensitiveLabel={caseSensitiveLabel}
          onFocus={handleSearchFocus}
          onBlur={handleSearchBlur}
        />
        <div className={resultsContainerClassName} style={containerStyle}>
          {activeTab === 'files' && volumes.length > 0 && (
            <div className="volume-selector" role="group" aria-label={t('volumes.scope')}>
              <span>{volume ? t('volumes.scope') : t('volumes.local')}</span>
              {volumes.map((v) => (
                <label key={v.id} title={v.path + ' · ' + v.id}>
                  <input
                    type="checkbox"
                    checked={v.id === volume?.id}
                    onChange={(e) => {
                      clearSelection();
                      closeQuickLook();
                      setVolumeCount(null);
                      selectVolume(e.target.checked ? v : null);
                    }}
                  />
                  {v.path.split('/').pop()}
                  {volumes.some((other) => other.id !== v.id && other.path === v.path) &&
                    ` (${v.id.slice(0, 8)})`}
                  {' · '}
                  {t(
                    v.online
                      ? v.indexing
                        ? 'volumes.indexing'
                        : 'volumes.online'
                      : 'volumes.offline',
                  )}
                  {!v.reusable && ' · ' + t('volumes.snapshot')}
                </label>
              ))}
            </div>
          )}
          {activeTab === 'events' && (
            <FSEventsPanel
              ref={eventsPanelRef}
              events={filteredEvents}
              onResizeStart={onEventResizeStart}
              onContextMenu={handleEventsContextMenu}
              onHeaderContextMenu={showEventsHeaderContextMenu}
              searchQuery={eventFilterQuery}
              caseInsensitive={!caseSensitive}
            />
          )}
          <div className="files-tab-pane" hidden={activeTab !== 'files'}>
            {volume ? (
              <VolumeFiles
                key={volume.id}
                volume={volume}
                exclusions={volumeExclusions}
                onExclude={excludePath}
                query={searchParams.query}
                directory={directoryScopeOpen ? directoryQuery : ''}
                caseSensitive={caseSensitive}
                onResizeStart={onResizeStart}
                onCount={setVolumeCount}
              />
            ) : (
              <FilesTabContent
                headerRef={headerRef}
                onResizeStart={onResizeStart}
                onHeaderContextMenu={showFilesHeaderContextMenu}
                displayState={displayState}
                searchErrorMessage={searchErrorMessage}
                currentQuery={currentQuery}
                currentDirectoryQuery={currentDirectoryQuery}
                virtualListRef={virtualListRef}
                results={displayedResults}
                pages={displayedPages}
                dataResultsVersion={displayedDataVersion}
                displayedResultsVersion={displayedResultsVersion}
                queryVersion={queryVersion}
                fileChanges={displayedFileChanges}
                changedIds={displayedChangedIds}
                rowHeight={ROW_HEIGHT}
                overscan={OVERSCAN_ROW_COUNT}
                renderRow={renderRow}
                onScrollSync={handleHorizontalSync}
                isSorting={isSorting}
                sortState={sortState}
                onSortToggle={handleSortToggle}
                sortDisabled={sortButtonsDisabled}
                sortDisabledTooltip={sortDisabledTooltip}
              />
            )}
          </div>
        </div>
        <StatusBar
          metadata={volume ? undefined : state.metadata}
          scannedFiles={volume ? (volume.count ?? volumeCount?.indexed ?? 0) : scannedFiles}
          processedEvents={processedEvents}
          lifecycleState={volume ? (volume.indexing ? 'Updating' : 'Ready') : lifecycleState}
          searchDurationMs={volume ? null : durationMs}
          resultCount={volume ? (volumeCount?.matched ?? null) : resultCount}
          activeTab={activeTab}
          onTabChange={onTabChange}
          onRequestRescan={
            volume
              ? () => {
                  void invoke('rescan_volume', { id: volume.id });
                }
              : requestRescan
          }
          canRescan={!volume || (volume.online && !volume.indexing)}
          rescanErrorCount={volume ? Number(!!volume.error) : rescanErrors}
        />
      </main>
      <PreferencesOverlay
        open={isPreferencesOpen}
        onClose={closePreferences}
        sortThreshold={sortThreshold}
        defaultSortThreshold={DEFAULT_SORTABLE_RESULT_THRESHOLD}
        onSortThresholdChange={setSortThreshold}
        trayIconEnabled={trayIconEnabled}
        onTrayIconEnabledChange={setTrayIconEnabled}
        watchRoot={watchRoot ?? defaultWatchRoot}
        defaultWatchRoot={defaultWatchRoot}
        onWatchConfigChange={handleWatchConfigChange}
        volumes={volumes}
        volumeExclusions={volumeExclusions}
        onVolumeExclusionsChange={setVolumeExclusions}
        ignorePaths={ignorePaths}
        defaultIgnorePaths={defaultIgnorePaths}
        includePaths={includePaths}
        defaultIncludePaths={defaultIncludePaths}
        onReset={handleResetPreferences}
        themeResetToken={preferencesResetToken}
      />
      {showFullDiskAccessOverlay && (
        <PermissionOverlay
          title={t('app.fullDiskAccess.title')}
          description={t('app.fullDiskAccess.description')}
          steps={permissionSteps}
          statusMessage={overlayStatusMessage}
          onRequestPermission={requestFullDiskAccessPermission}
          disabled={isCheckingFullDiskAccess}
          actionLabel={openSettingsLabel}
        />
      )}
    </>
  );
}

export default App;
