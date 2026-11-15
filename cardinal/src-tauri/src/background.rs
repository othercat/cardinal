use crate::{
    commands::{
        NodeInfoRequest, SearchJob, SearchResponse, SortJob, SortedEdits, WatchConfigUpdate,
    },
    lifecycle::{APP_QUIT, AppLifecycleState, load_app_state, update_app_state},
    search_activity,
    window_controls::is_main_window_foreground,
};
use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
use cardinal_sdk::{EventFlag, EventWatcher, FsEvent};
use crossbeam_channel::{Receiver, Sender};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use rayon::spawn;
use search_cache::{
    EventBuffer, EventReplay, InitialScan, LiveSearch, SearchCache, SearchOptions,
    SearchResultNode, SlabIndex, SortDirection, SortKey, VIEW_PAGE_SIZE, WalkData,
};
use search_cancel::CancellationToken;
use serde::Serialize;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};
use tracing::{error, info};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StatusBarUpdate {
    pub scanned_files: usize,
    pub processed_events: usize,
    pub rescan_errors: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<MetadataProgress>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct IconPayload {
    pub path: String,
    pub slab_index: SlabIndex,
    pub icon: String,
    pub thumbnail: bool,
}

pub struct BackgroundLoopChannels {
    pub finish_rx: Receiver<Sender<Option<SearchCache>>>,
    pub update_window_state_rx: Receiver<()>,
    pub search_rx: Receiver<SearchJob>,
    pub node_info_rx: Receiver<NodeInfoRequest>,
    pub sort_rx: Receiver<SortJob>,
    pub icon_viewport_rx: Receiver<(u64, Vec<SlabIndex>)>,
    pub rescan_rx: Receiver<CancellationToken>,
    pub watch_config_rx: Receiver<WatchConfigUpdate>,
    pub icon_update_tx: Sender<IconPayload>,
}

#[derive(Clone)]
struct SortedOrder {
    missing: usize,
    view: u64,
    key: SortKey,
    ids: Arc<Vec<SlabIndex>>,
}

#[derive(Default)]
struct PublishedOrders {
    latest: Option<SortedOrder>,
    previous: Option<SortedOrder>,
}

impl PublishedOrders {
    fn get(&self, view: u64, key: SortKey) -> Option<&SortedOrder> {
        [&self.latest, &self.previous]
            .into_iter()
            .filter_map(Option::as_ref)
            .find(|o| o.view == view && o.key == key)
    }

    fn publish(&mut self, order: SortedOrder) {
        self.previous = self.latest.replace(order);
    }
}

#[derive(Clone, Copy, Serialize)]
struct MetadataProgress {
    processed: usize,
    total: usize,
    revision: u64,
}

struct Hydration {
    cancel: Arc<AtomicBool>,
    pending: Option<search_cache::MetadataPlanner>,
    progress: MetadataProgress,
    emitted: Instant,
    published: Instant,
}

impl Drop for Hydration {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

// Slow File Provider volumes must not occupy the interactive sort / event workers.
static METADATA_WORKERS: Lazy<rayon::ThreadPool> = Lazy::new(|| {
    rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 6)))
        .thread_name(|i| format!("cardinal-attributes-{i}"))
        .build()
        .expect("metadata workers")
});

pub fn reset_status_bar(app_handle: &AppHandle) {
    app_handle
        .emit(
            "status_bar_update",
            StatusBarUpdate {
                scanned_files: 0,
                processed_events: 0,
                rescan_errors: 0,
                metadata: None,
            },
        )
        .unwrap();
}

pub fn emit_status_bar_update(
    app_handle: &AppHandle,
    scanned_files: usize,
    processed_events: usize,
    rescan_errors: usize,
) {
    static LAST_EMIT: Lazy<Mutex<Instant>> =
        Lazy::new(|| Mutex::new(Instant::now() - Duration::from_secs(1)));

    {
        let mut last_emit = LAST_EMIT.lock();
        if Instant::now().duration_since(*last_emit) < Duration::from_millis(100) {
            return;
        }
        app_handle
            .emit(
                "status_bar_update",
                StatusBarUpdate {
                    scanned_files,
                    processed_events,
                    rescan_errors,
                    metadata: None,
                },
            )
            .unwrap();
        *last_emit = Instant::now();
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_watch_config_update(
    app_handle: &AppHandle,
    update: WatchConfigUpdate,
    cache: &mut SearchCache,
    event_watcher: &mut EventWatcher,
    watch_root: &mut String,
    fse_latency_secs: f64,
    history_ready: &mut bool,
    processed_events: &mut usize,
) -> bool {
    info!("Received watch config update: {:?}", update);
    let WatchConfigUpdate {
        watch_root: next_watch_root,
        ignore_paths,
        include_paths,
        scan_cancellation_token,
    } = update;

    let next_ignore_paths = ignore_paths
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let next_include_paths = include_paths
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    *event_watcher = EventWatcher::noop();
    update_app_state(app_handle, AppLifecycleState::Initializing);
    reset_status_bar(app_handle);
    *history_ready = false;
    *processed_events = 0;

    let next_cache = match build_search_cache(
        app_handle,
        &next_watch_root,
        &next_ignore_paths,
        &next_include_paths,
        scan_cancellation_token,
    ) {
        Some(cache) => {
            info!(
                "Search cache built. New root: {}, ignore paths: {:?}, include paths: {:?}",
                next_watch_root, next_ignore_paths, next_include_paths
            );
            emit_status_bar_update(app_handle, cache.get_total_files(), 0, 0);
            cache
        }
        None => {
            // if cache build is cancelled, we cannot reuse the old cache since
            // it's tied to the old watch config; create a noop cache instead
            info!("Watch config change cancelled, use noop state");
            SearchCache::noop(
                PathBuf::from(&next_watch_root),
                next_ignore_paths,
                next_include_paths,
                &APP_QUIT,
            )
        }
    };

    *cache = next_cache;
    *watch_root = next_watch_root.to_string();
    *event_watcher = if cache.is_noop() {
        EventWatcher::noop()
    } else {
        update_app_state(app_handle, AppLifecycleState::Updating);
        EventWatcher::spawn(
            watch_root.to_string(),
            cache.last_event_id(),
            fse_latency_secs,
            cache.ignore_paths(),
            cache.include_paths(),
        )
        .1
    };
    !cache.is_noop()
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RecentEvent {
    path: String,
    flag_bits: u32,
    event_id: u64,
    timestamp: i64,
}

impl RecentEvent {
    fn from_event(event: &FsEvent, timestamp: i64) -> Self {
        Self {
            path: event.path.to_string_lossy().into_owned(),
            flag_bits: event.flag.bits(),
            event_id: event.id,
            timestamp,
        }
    }
}

// The Events tab keeps the last 10,000 actual notifications in receipt order,
// independently of when the Files tab sees their reconciled metadata.
#[derive(Default)]
struct RecentEvents(VecDeque<RecentEvent>);

impl RecentEvents {
    fn record(&mut self, incoming: &[FsEvent], mut live: bool) {
        let timestamp = unix_timestamp_now();
        for event in incoming {
            if event.flag.contains(EventFlag::HistoryDone) {
                live = true;
            } else if live {
                if self.0.len() == 10_000 {
                    self.0.pop_front();
                }
                self.0.push_back(RecentEvent::from_event(event, timestamp));
            }
        }
    }
}

#[derive(Serialize, Clone)]
struct EventUpdate<'a> {
    recent: &'a VecDeque<RecentEvent>,
    // Normally the same events feed both views, so serialize them only once.
    #[serde(skip_serializing_if = "Option::is_none")]
    indexed: Option<Vec<RecentEvent>>,
}

fn handle_flush_tick(
    app_handle: &AppHandle,
    cache: &mut SearchCache,
    db_path: &Path,
    hide_flush_remaining_ticks: &mut u8,
    complete_index: bool,
) {
    if !complete_index || cache.is_noop() {
        return;
    }
    let mut flush_search_cache = FlushSearchCache { cache, db_path };
    let flushed = start_flush_checks(
        || is_main_window_foreground(app_handle),
        search_activity::search_idles,
        &mut flush_search_cache,
        hide_flush_remaining_ticks,
    );
    if flushed {
        search_activity::note_search_activity();
    }
}

fn publish_applied_events(
    app_handle: &AppHandle,
    cache: &mut SearchCache,
    events: Vec<FsEvent>,
    history_ready: &mut bool,
    processed_events: usize,
    recent: &mut RecentEvents,
) {
    let indexed = if *history_ready {
        &events[..]
    } else if let Some(pos) = events
        .iter()
        .position(|e| e.flag.contains(EventFlag::HistoryDone))
    {
        *history_ready = true;
        &events[pos + 1..]
    } else {
        &[]
    };
    emit_status_bar_update(
        app_handle,
        cache.get_total_files(),
        processed_events,
        cache.rescan_count() as usize,
    );
    if *history_ready {
        update_app_state(app_handle, AppLifecycleState::Ready);
        if !indexed.is_empty() || !recent.0.is_empty() {
            let same = indexed.len() == recent.0.len()
                && indexed.iter().zip(&recent.0).all(|(e, r)| {
                    e.id == r.event_id
                        && e.flag.bits() == r.flag_bits
                        && e.path.as_os_str() == r.path.as_str()
                });
            let timestamp = unix_timestamp_now();
            let _ = app_handle.emit(
                "fs_events_batch",
                EventUpdate {
                    recent: &recent.0,
                    indexed: (!same).then(|| {
                        indexed
                            .iter()
                            .map(|e| RecentEvent::from_event(e, timestamp))
                            .collect()
                    }),
                },
            );
            recent.0.clear();
        }
    }
}

fn handle_icon_viewport_update(
    cache: &mut SearchCache,
    update: (u64, Vec<SlabIndex>),
    icon_update_tx: &Sender<IconPayload>,
) {
    let (_request_id, viewport) = update;

    let nodes = cache.cached_file_nodes(&viewport);
    let icon_jobs: Vec<_> = viewport
        .into_iter()
        .zip(nodes)
        .map(|(slab_index, SearchResultNode { path, .. })| (slab_index, path))
        .collect();

    if icon_jobs.is_empty() {
        return;
    }

    icon_jobs
        .into_iter()
        .map(|(slab_index, path)| (slab_index, path.to_string_lossy().into_owned()))
        .for_each(|(slab_index, path)| {
            let icon_update_tx = icon_update_tx.clone();
            spawn(move || {
                let cloud = ["OneDrive", "com~apple~CloudDocs", "Google Drive", "Dropbox"]
                    .iter()
                    .any(|marker| path.contains(marker));
                let preview = (!cloud).then(|| fs_icon::icon_of_path_ql(&path)).flatten();
                let thumbnail = preview.is_some();
                let data = preview.or_else(|| fs_icon::icon_of_path_ns(&path));
                if let Some(icon) = data.map(|data| {
                    format!(
                        "data:image/png;base64,{}",
                        general_purpose::STANDARD.encode(&data)
                    )
                }) {
                    let _ = icon_update_tx.send(IconPayload {
                        slab_index,
                        path,
                        icon,
                        thumbnail,
                    });
                }
            });
        });
}

#[allow(clippy::too_many_arguments)]
pub fn run_background_event_loop(
    app_handle: &AppHandle,
    mut cache: SearchCache,
    mut event_watcher: EventWatcher,
    mut initial_scan: Option<InitialScan>,
    channels: BackgroundLoopChannels,
    mut watch_root: String,
    fse_latency_secs: f64,
    db_path: PathBuf,
) {
    let BackgroundLoopChannels {
        finish_rx,
        update_window_state_rx,
        search_rx,
        node_info_rx,
        sort_rx,
        icon_viewport_rx,
        rescan_rx,
        watch_config_rx,
        icon_update_tx,
    } = channels;
    let mut live = LiveSearch::default();
    let (hydrated_tx, hydrated_rx) = crossbeam_channel::bounded(4);
    let sorted_order = Arc::new(Mutex::new(PublishedOrders::default()));
    let mut hydration_epoch = 0u64;
    let mut metadata_revision = 0u64;
    let mut hydration: Option<Hydration> = None;
    let no_plan = crossbeam_channel::never();
    let mut plan_timer = no_plan.clone();
    let mut processed_events = 0usize;
    let mut history_ready = load_app_state() == AppLifecycleState::Ready;
    // A loaded snapshot is a complete baseline even while its history is
    // replaying. A cold scan must finish before it may replace that snapshot.
    let mut complete_index = initial_scan.is_none() && !cache.is_noop();

    let mut window_is_foreground = true;
    let mut hide_flush_remaining_ticks: u8 = 0;
    // Hide flush is polled on a 10s ticker; idle flush shares the same tick.
    let flush_ticker = crossbeam_channel::tick(Duration::from_secs(10));

    let idle_scan = crossbeam_channel::never();
    let no_events = crossbeam_channel::never();
    let no_observations = crossbeam_channel::never();
    let mut replay: Option<(EventReplay, Vec<FsEvent>)> = None;
    let mut buffered_events = EventBuffer::default();
    let mut recent = RecentEvents::default();
    loop {
        complete_index |= load_app_state() == AppLifecycleState::Ready;
        if APP_QUIT.load(Ordering::Relaxed) && !complete_index {
            return;
        }
        let scan_rx = initial_scan
            .as_ref()
            .map(InitialScan::batches)
            .unwrap_or(&idle_scan);
        let events_rx: &Receiver<_> = if replay.is_some() {
            &no_events
        } else {
            &event_watcher
        };
        let observations = replay
            .as_ref()
            .map(|(r, _)| r.observations())
            .unwrap_or(&no_observations);
        let deadline = buffered_events
            .deadline()
            .filter(|_| replay.is_none())
            .map(crossbeam_channel::at);
        let mut incoming = None;
        crossbeam_channel::select! {
            recv(scan_rx) -> batch => {
                let scan = initial_scan.as_mut().expect("active scan");
                if scan.cancelled() {
                    initial_scan = None;
                    // A queued rescan/config change replaces this incomplete tree.
                    continue;
                }
                let done = scan.apply(&mut cache, batch.expect("scan channel closed"));
                emit_status_bar_update(app_handle, cache.get_total_files(), 0, 0);
                if done {
                    initial_scan = None;
                    complete_index = true;
                    update_app_state(app_handle, AppLifecycleState::Updating);
                    event_watcher = EventWatcher::spawn(
                        watch_root.clone(),
                        cache.last_event_id(),
                        fse_latency_secs,
                        cache.ignore_paths(),
                        cache.include_paths(),
                    )
                    .1;
                }
            }
            recv(finish_rx) -> tx => {
                if let Ok(tx) = tx {
                    // A busy replay keeps its last complete cursor for next launch.
                    if replay.is_none() && let Some(due) = buffered_events.deadline()
                        && let Ok(Some((plan, _))) = buffered_events.prepare(&mut cache, vec![], due)
                    {
                        plan.read(|| false, |change| { cache.apply_observation(change); });
                    }
                    // Partial event observations retain the last complete batch's
                    // cursor, so they can be saved and replayed after restart.
                    let _ = tx.send((complete_index && !cache.is_noop()).then_some(cache));
                }
                return;
            }
            recv(update_window_state_rx) -> _ => {
                // Recompute foreground state on demand instead of mirroring events.
                let new_foreground = is_main_window_foreground(app_handle);
                if window_is_foreground && !new_foreground {
                    hide_flush_remaining_ticks = 2; // allow 10~20s before running hide flush
                } else if new_foreground {
                    hide_flush_remaining_ticks = 0;
                }
                window_is_foreground = new_foreground;
            }
            recv(flush_ticker) -> _ => {
                handle_flush_tick(
                    app_handle,
                    &mut cache,
                    &db_path,
                    &mut hide_flush_remaining_ticks,
                    complete_index,
                );
            }
            recv(search_rx) -> job => {
                let SearchJob {
                    query,
                    options,
                    base_view_id,
                    sort_key,
                    cancellation_token,
                    result_tx,
                } = job.expect("Search channel closed");
                let opts = SearchOptions::from(options);
                let payload = live
                    .search_windowed(&mut cache, query, opts, base_view_id, cancellation_token)
                    .map(|view| {
                        let mut cached = sorted_order.lock();
                        let mut sorted = None;
                        if let Some(order) = cached.latest.as_ref()
                            && order.view != view.view_id
                            && order.missing == 0
                            && Some(order.key) == sort_key
                            && Some(order.view) == base_view_id
                            && let Some((dirty, members)) = live.delta_since(order.view)
                        {
                            // Keep the last published order readable until its replacement
                            // is actually visible. A page request can cross a search reply.
                            let mut next = order.clone();
                            if let Some(edits) = cache.patch_sorted_order(
                                Arc::make_mut(&mut next.ids),
                                next.key,
                                dirty,
                                members,
                                cancellation_token,
                            ) {
                                sorted = Some(SortedEdits {
                                    base_view_id: next.view,
                                    key: next.key,
                                    edits,
                                });
                                next.view = view.view_id;
                                cached.publish(next);
                            }
                        }
                        SearchResponse { view, sorted }
                    });
                let _ = result_tx.send(payload);
            }
            recv(plan_timer) -> _ => {
                plan_timer = no_plan.clone();
                if let Some(work) = &mut hydration
                    && let Some(planner) = &mut work.pending
                {
                    let Some(plan) = planner.advance(&cache) else {
                        plan_timer = crossbeam_channel::after(Duration::from_millis(1));
                        continue;
                    };
                    work.pending = None;
                    let cancel = Arc::clone(&work.cancel);
                    let tx = hydrated_tx.clone();
                    let epoch = hydration_epoch;
                    METADATA_WORKERS.spawn(move || {
                        plan.run(
                            || APP_QUIT.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed),
                            |rows| {
                                let _ = tx.send((epoch, Some(rows)));
                            },
                        );
                        let _ = tx.send((epoch, None));
                    });
                }
            }
            recv(hydrated_rx) -> next => {
                if let Ok((epoch, rows)) = next
                    && epoch == hydration_epoch
                    && let Some(work) = &mut hydration
                {
                    let done = rows.is_none();
                    if let Some(rows) = rows {
                        work.progress.processed += rows.len();
                        cache.remember_hydrated_rows(rows);
                    }
                    if done || work.emitted.elapsed() >= Duration::from_millis(500) {
                        if done || work.published.elapsed() >= Duration::from_secs(4) {
                            cache.publish_hydrated_metadata();
                            metadata_revision += 1;
                            work.progress.revision = metadata_revision;
                            work.published = Instant::now();
                        }
                        if done {
                            work.progress.processed = work.progress.total;
                        }
                        let _ = app_handle.emit(
                            "status_bar_update",
                            StatusBarUpdate {
                                scanned_files: cache.get_total_files(),
                                processed_events,
                                rescan_errors: cache.rescan_count() as usize,
                                metadata: Some(work.progress),
                            },
                        );
                        work.emitted = Instant::now();
                    }
                    if done {
                        hydration = None;
                    }
                }
            }
            recv(node_info_rx) -> request => {
                match request.expect("Node info channel closed") {
                    NodeInfoRequest::Rows {
                        slab_indices,
                        response_tx,
                    } => {
                        let _ = response_tx.send(cache.expand_file_nodes(&slab_indices));
                    }
                    NodeInfoRequest::Page {
                        view_id,
                        sort,
                        offset,
                        limit,
                        locate,
                        response_tx,
                    } => {
                        let descending = sort.is_some_and(|s| s.direction == SortDirection::Desc);
                        let stored = sorted_order.lock();
                        let result = if let Some(order) =
                            sort.and_then(|s| stored.get(view_id, s.key))
                        {
                            Some(view_page(
                                &order.ids,
                                order.missing,
                                descending,
                                offset,
                                limit,
                                locate.as_deref(),
                            ))
                        } else if let Some(raw) = live.nodes(view_id) {
                            match sort {
                                None => {
                                    Some(view_page(raw, 0, false, offset, limit, locate.as_deref()))
                                }
                                Some(sort)
                                    if sort.key == SortKey::Filename && live.name_ordered() =>
                                {
                                    Some(match locate {
                                        None => live
                                            .filename_page(&mut cache, offset, limit, descending)
                                            .into_iter()
                                            .map(|id| id.get() as u32)
                                            .collect(),
                                        Some(ids) => {
                                            live.filename_positions(&mut cache, &ids, descending)
                                        }
                                    })
                                }
                                _ => None,
                            }
                        } else {
                            None
                        };
                        let _ = response_tx.send(result);
                    }
                }
            }
            recv(sort_rx) -> job => {
                let SortJob {
                    view_id,
                    paged,
                    slab_indices,
                    sort,
                    cancellation_token,
                    response_tx,
                } = job.expect("Sort channel closed");
                let source = match view_id {
                    Some(id) => live.nodes(id),
                    None => Some(slab_indices.as_slice()),
                };
                let Some(ids) = source else {
                    let _ = response_tx.send(None);
                    continue;
                };
                if paged && sort.key == SortKey::Filename && live.name_ordered() {
                    let _ = response_tx.send(Some(live.filename_page(
                        &mut cache,
                        0,
                        VIEW_PAGE_SIZE,
                        sort.direction == SortDirection::Desc,
                    )));
                    continue;
                }
                let head = move |order: &SortedOrder| -> Vec<SlabIndex> {
                    view_page(
                        &order.ids,
                        order.missing,
                        sort.direction == SortDirection::Desc,
                        0,
                        VIEW_PAGE_SIZE,
                        None,
                    )
                    .into_iter()
                    .map(|id| SlabIndex::new(id as usize))
                    .collect()
                };
                if paged {
                    if let Some(cached) =
                        view_id.and_then(|v| sorted_order.lock().get(v, sort.key).cloned())
                    {
                        let _ = response_tx.send(Some(head(&cached)));
                        continue;
                    }
                    if sort.key == SortKey::FullPath
                        && ids.len().saturating_mul(8) >= cache.get_total_files()
                    {
                        let page = cache.path_order(ids, cancellation_token).and_then(|ids| {
                            let order = SortedOrder {
                                view: view_id?,
                                key: sort.key,
                                missing: 0,
                                ids: Arc::new(ids),
                            };
                            let page = head(&order);
                            sorted_order.lock().publish(order);
                            Some(page)
                        });
                        let _ = response_tx.send(page);
                        continue;
                    }
                    let numeric = matches!(sort.key, SortKey::Size | SortKey::Mtime | SortKey::Ctime);
                    let ranked = numeric
                        .then(|| {
                            cache.indexed_numeric_order(
                                ids,
                                sort.key,
                                live.name_ordered(),
                                cancellation_token,
                            )
                        })
                        .flatten();
                    let text = if numeric {
                        None
                    } else {
                        cache.prepare_sort(ids, sort.key, cancellation_token)
                    };
                    let missing = ranked.as_ref().map_or(0, |r| r.missing);
                    let to_hydrate = ranked.as_ref().map_or(0, |r| r.to_hydrate);
                    if to_hydrate > 0 && hydration.is_none() {
                        hydration_epoch += 1;
                        let progress = MetadataProgress {
                            processed: 0,
                            total: to_hydrate,
                            revision: metadata_revision,
                        };
                        let _ = app_handle.emit(
                            "status_bar_update",
                            StatusBarUpdate {
                                scanned_files: cache.get_total_files(),
                                processed_events,
                                rescan_errors: cache.rescan_count() as usize,
                                metadata: Some(progress),
                            },
                        );
                        hydration = Some(Hydration {
                            cancel: Arc::new(AtomicBool::new(false)),
                            pending: Some(search_cache::MetadataPlanner::new(ids)),
                            progress,
                            emitted: Instant::now(),
                            published: Instant::now(),
                        });
                        plan_timer = crossbeam_channel::after(Duration::from_millis(120));
                    }
                    let stored = Arc::clone(&sorted_order);
                    spawn(move || {
                        let ids = if let Some(ranked) = ranked {
                            ranked.sort(cancellation_token)
                        } else {
                            text.and_then(|s| s.sort(SortDirection::Asc, cancellation_token))
                        };
                        let page = ids.and_then(|ids| {
                            let order = SortedOrder {
                                view: view_id?,
                                key: sort.key,
                                missing,
                                ids: Arc::new(ids),
                            };
                            let mut guard = stored.lock();
                            cancellation_token.is_cancelled()?;
                            let page = head(&order);
                            guard.publish(order);
                            Some(page)
                        });
                        let _ = response_tx.send(page);
                    });
                } else {
                    let snapshot = cache.prepare_sort(ids, sort.key, cancellation_token);
                    let stored = Arc::clone(&sorted_order);
                    spawn(move || {
                        let result =
                            snapshot.and_then(|s| s.sort(sort.direction, cancellation_token));
                        if let (Some(view), Some(ids)) = (view_id, &result)
                            && sort.direction == SortDirection::Asc
                        {
                            let mut order = stored.lock();
                            if cancellation_token.is_cancelled().is_some() {
                                order.publish(SortedOrder {
                                    view,
                                    key: sort.key,
                                    missing: 0,
                                    ids: Arc::new(ids.clone()),
                                });
                            }
                        }
                        let _ = response_tx.send(result);
                    });
                }
            }
            recv(icon_viewport_rx) -> update => {
                let update = update.expect("Icon viewport channel closed");
                handle_icon_viewport_update(&mut cache, update, &icon_update_tx);
            }
            recv(rescan_rx) -> request => {
                let scan_cancellation_token = request.expect("Rescan channel closed");
                if scan_cancellation_token.is_cancelled().is_none() {
                    continue;
                }
                initial_scan = None;
                replay = None;
                buffered_events = EventBuffer::default();
                recent.0.clear();
                hydration_epoch += 1;
                hydration = None;
                info!("Manual rescan requested");
                complete_index |= perform_rescan(
                    app_handle,
                    &mut cache,
                    &mut event_watcher,
                    &watch_root,
                    fse_latency_secs,
                    &mut history_ready,
                    &mut processed_events,
                    scan_cancellation_token,
                );
            }
            recv(watch_config_rx) -> update => {
                initial_scan = None;
                replay = None;
                buffered_events = EventBuffer::default();
                recent.0.clear();
                hydration_epoch += 1;
                hydration = None;
                let next_update = update.expect("Watch config channel closed");
                complete_index = handle_watch_config_update(
                    app_handle,
                    next_update,
                    &mut cache,
                    &mut event_watcher,
                    &mut watch_root,
                    fse_latency_secs,
                    &mut history_ready,
                    &mut processed_events,
                );
            }
            recv(observations) -> observation => {
                let Ok(observation) = observation else {
                    // The worker can be cancelled while quitting. Keep the last complete cursor.
                    replay = None;
                    continue;
                };
                if cache.apply_observation(observation) {
                    let (_, events) = replay.take().expect("active event batch");
                    publish_applied_events(
                        app_handle,
                        &mut cache,
                        events,
                        &mut history_ready,
                        processed_events,
                        &mut recent,
                    );
                }
            }
            recv(events_rx) -> events => {
                let mut events = events.expect("Event stream closed");
                for more in event_watcher.try_iter().take(7) {
                    events.extend(more);
                }
                incoming = Some(events);
            }
            recv(deadline.as_ref().unwrap_or(&no_plan)) -> _ => {
                incoming = Some(vec![]);
            }
        }
        if let Some(events) = incoming {
            processed_events += events.len();
            recent.record(&events, history_ready);
            let work = if history_ready {
                buffered_events.prepare(&mut cache, events, Instant::now())
            } else {
                cache.plan_events(&events).map(|plan| Some((plan, events)))
            };
            match work {
                Ok(Some((plan, events))) => replay = Some((plan.start(&APP_QUIT), events)),
                Ok(None) => {}
                Err(_) => {
                    buffered_events = EventBuffer::default();
                    recent.0.clear();
                    hydration_epoch += 1;
                    hydration = None;
                    complete_index |= perform_rescan(
                        app_handle,
                        &mut cache,
                        &mut event_watcher,
                        &watch_root,
                        fse_latency_secs,
                        &mut history_ready,
                        &mut processed_events,
                        CancellationToken::new_scan(),
                    );
                }
            }
        }
    }
}

pub(crate) fn view_page(
    ids: &[SlabIndex],
    missing: usize,
    desc: bool,
    offset: usize,
    limit: usize,
    locate: Option<&[SlabIndex]>,
) -> Vec<u32> {
    let known = ids.len().saturating_sub(missing);
    // Rows waiting for an attribute follow all known values in either direction.
    let position = |i| if desc && i < known { known - 1 - i } else { i };
    if let Some(wanted) = locate {
        let mut positions: std::collections::HashMap<_, _> =
            wanted.iter().map(|&id| (id, u32::MAX)).collect();
        for (i, id) in ids.iter().enumerate() {
            if let Some(pos) = positions.get_mut(id) {
                *pos = position(i) as u32;
            }
        }
        return wanted.iter().map(|id| positions[id]).collect();
    }
    let a = offset.min(ids.len());
    (a..a + limit.min(ids.len() - a))
        .map(|i| ids[position(i)].get() as u32)
        .collect()
}

pub(crate) fn build_search_cache(
    app_handle: &AppHandle,
    watch_root: &str,
    ignore_paths: &[PathBuf],
    include_paths: &[PathBuf],
    scan_cancellation_token: CancellationToken,
) -> Option<SearchCache> {
    let path = Path::new(watch_root);
    let walk_data = WalkData::new(path, ignore_paths, include_paths, true, move || {
        APP_QUIT.load(Ordering::Relaxed) || scan_cancellation_token.is_cancelled().is_none()
    });
    let walking_done = AtomicBool::new(false);

    std::thread::scope(|s| {
        s.spawn(|| {
            while !walking_done.load(Ordering::Relaxed) {
                let dirs = walk_data.num_dirs.load(Ordering::Relaxed);
                let files = walk_data.num_files.load(Ordering::Relaxed);
                let total = dirs + files;
                emit_status_bar_update(app_handle, total, 0, 0);
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let cache = SearchCache::walk_fs_with_walk_data(&walk_data, &APP_QUIT);
        walking_done.store(true, Ordering::Relaxed);
        cache
    })
}

#[allow(clippy::too_many_arguments)]
fn perform_rescan(
    app_handle: &AppHandle,
    cache: &mut SearchCache,
    event_watcher: &mut EventWatcher,
    watch_root: &str,
    fse_latency_secs: f64,
    history_ready: &mut bool,
    processed_events: &mut usize,
    scan_cancellation_token: CancellationToken,
) -> bool {
    if scan_cancellation_token.is_cancelled().is_none() {
        info!("Skipping stale rescan request");
        return false;
    }

    *event_watcher = EventWatcher::noop();
    update_app_state(app_handle, AppLifecycleState::Initializing);
    *history_ready = false;
    *processed_events = 0;
    reset_status_bar(app_handle);

    let mut phantom1 = PathBuf::new();
    let mut phantom2 = Vec::new();
    let mut phantom3 = Vec::new();
    let walk_data = cache.walk_data(
        &mut phantom1,
        &mut phantom2,
        &mut phantom3,
        scan_cancellation_token,
    );
    let walking_done = AtomicBool::new(false);
    let stopped = std::thread::scope(|s| {
        s.spawn(|| {
            while !walking_done.load(Ordering::Relaxed) {
                let dirs = walk_data.num_dirs.load(Ordering::Relaxed);
                let files = walk_data.num_files.load(Ordering::Relaxed);
                let total = dirs + files;
                emit_status_bar_update(app_handle, total, 0, 0);
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        // If rescan is cancelled, we have nothing to do
        let stopped = cache.rescan_with_walk_data(&walk_data).is_none();
        walking_done.store(true, Ordering::Relaxed);
        stopped
    });

    *event_watcher = if stopped {
        EventWatcher::noop()
    } else {
        update_app_state(app_handle, AppLifecycleState::Updating);
        EventWatcher::spawn(
            watch_root.to_string(),
            cache.last_event_id(),
            fse_latency_secs,
            cache.ignore_paths(),
            cache.include_paths(),
        )
        .1
    };
    !stopped
}

fn unix_timestamp_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

struct FlushSearchCache<'cache> {
    cache: &'cache mut SearchCache,
    db_path: &'cache Path,
}

trait FlushSnapshot {
    fn flush_snapshot_to_file(&mut self) -> Result<()>;
    fn db_path(&self) -> &Path;
}

impl FlushSnapshot for FlushSearchCache<'_> {
    fn flush_snapshot_to_file(&mut self) -> Result<()> {
        SearchCache::flush_snapshot_to_file(self.cache, self.db_path)
    }
    fn db_path(&self) -> &Path {
        self.db_path
    }
}

/// This function should be called periodically to check if a flush is needed.
/// Returns true only when a snapshot was saved successfully.
fn start_flush_checks<F, I, C>(
    is_foreground: F,
    is_idle: I,
    cache: &mut C,
    hide_flush_remaining_ticks: &mut u8,
) -> bool
where
    F: Fn() -> bool,
    I: Fn() -> bool,
    C: FlushSnapshot,
{
    let idle_flush = is_idle();
    let hide_flush = {
        // Consume the pending hide flush counter; only fire once.
        if *hide_flush_remaining_ticks > 0 {
            *hide_flush_remaining_ticks -= 1;
            *hide_flush_remaining_ticks == 0
        } else {
            false
        }
    };
    let hide_flush = hide_flush && !is_foreground();

    let label = if hide_flush {
        "hide_flush"
    } else if idle_flush {
        "idle_flush"
    } else {
        return false;
    };
    match cache.flush_snapshot_to_file() {
        Ok(()) => {
            info!(
                "Cache flushed successfully ({label}) to {:?}",
                cache.db_path()
            );
            true
        }
        Err(e) => {
            error!(
                "Cache flush failed ({label}) to {:?}: {e:?}",
                cache.db_path()
            );
            if hide_flush {
                *hide_flush_remaining_ticks = 1;
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_pages_keep_unknown_values_last_in_both_directions() {
        let ids = [13, 7, 8, 4, 5].map(SlabIndex::new);
        assert_eq!(view_page(&ids, 2, false, 0, 5, None), [13, 7, 8, 4, 5]);
        assert_eq!(view_page(&ids, 2, true, 0, 5, None), [8, 7, 13, 4, 5]);
        assert_eq!(view_page(&ids, 2, true, 2, 2, None), [13, 4]);
        let locate = [4, 8, 13, 99].map(SlabIndex::new);
        assert_eq!(
            view_page(&ids, 2, true, 0, 0, Some(&locate)),
            [3, 0, 2, u32::MAX]
        );
        assert!(view_page(&ids, 2, true, usize::MAX, usize::MAX, None).is_empty());
    }

    #[derive(Default)]
    struct FakeCache {
        pub flushes: usize,
    }

    impl FlushSnapshot for FakeCache {
        fn flush_snapshot_to_file(&mut self) -> Result<()> {
            self.flushes += 1;
            Ok(())
        }

        fn db_path(&self) -> &Path {
            Path::new("db")
        }
    }

    #[test]
    fn hide_flush_resets_idle_window() {
        let mut cache = FakeCache::default();
        let mut pending = 1;
        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);

        assert_eq!(cache.flushes, 1, "hide flush should run once");
        assert_eq!(pending, 0, "pending hide flush should be consumed");
        assert!(
            flushed,
            "hide flush should satisfy idle window to avoid immediate idle flush"
        );
    }

    #[test]
    fn idle_flush_runs_when_due() {
        let mut cache = FakeCache::default();
        let mut pending = 0;
        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);

        assert_eq!(cache.flushes, 1, "idle flush should run once when due");
        assert!(flushed, "idle flush should advance idle window");
    }

    #[test]
    fn pending_consumed_but_no_hide_flush_if_foreground() {
        let mut cache = FakeCache::default();
        let mut pending = 1;
        // Foreground -> pending should be consumed but no hide flush should run
        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush satisfied in foreground");

        assert_eq!(
            cache.flushes, 0,
            "no flush should run while window is foreground"
        );
        assert_eq!(
            pending, 0,
            "pending counter should be consumed even if foreground"
        );
    }

    #[test]
    fn hide_flush_requires_two_ticks() {
        // make sure idle flush is not accidentally due from other tests

        let mut cache = FakeCache::default();
        let mut pending = 2;

        // First tick consumes one counter, no flush yet
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush satisfied on first tick");
        assert_eq!(cache.flushes, 0, "no flush on first tick");
        assert_eq!(pending, 1, "pending decremented to 1");

        // Second tick should trigger the hide flush
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "hide flush was performed");
        assert_eq!(cache.flushes, 1, "flush should run on second tick");
        assert_eq!(pending, 0, "pending should be consumed after flush");
    }

    #[test]
    fn hide_preempts_idle_and_only_one_flush_when_both_pending() {
        let mut cache = FakeCache::default();
        let mut pending = 1;

        // When both an idle flush is due and a hide flush fires, hide flush
        // should run and it should satisfy the idle window so we don't double-flush.
        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);

        assert_eq!(cache.flushes, 1, "only one flush should run");
        assert_eq!(pending, 0, "pending hide flush should be consumed");
        assert!(flushed, "hide flush should satisfy idle window");
    }

    use anyhow::anyhow;

    struct FakeCacheErr;

    impl FlushSnapshot for FakeCacheErr {
        fn flush_snapshot_to_file(&mut self) -> Result<()> {
            Err(anyhow!("flush failed"))
        }

        fn db_path(&self) -> &Path {
            Path::new("db")
        }
    }

    #[test]
    fn flush_error_does_not_reset_idle() {
        let mut cache = FakeCacheErr;
        let mut pending = 1;

        // A failed checkpoint keeps the idle deadline and retries the hide save.
        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);

        assert!(
            !flushed,
            "failed hide flush must not advance the idle window"
        );
        assert_eq!(pending, 1, "failed hide save should retry on the next tick");
    }

    // ===== Tests for foreground -> background transitions =====

    #[test]
    fn foreground_to_background_sets_pending_to_2() {
        // This test verifies the initial state when window goes to background
        // In the real event loop, `hide_flush_remaining_ticks` is set to 2
        // Note: if idle is already due, idle flush will trigger regardless of pending

        let mut cache = FakeCache::default();
        let mut pending = 2; // simulates what happens when window loses focus

        // First tick after going to background: pending=2, decrements to 1, no hide flush yet
        // idle not due, so no flush at all
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush on first tick");
        assert_eq!(
            cache.flushes, 0,
            "no flush on first tick after backgrounding (idle not due)"
        );
        assert_eq!(pending, 1, "pending should decrement from 2 to 1");
    }

    #[test]
    fn foreground_to_background_idle_not_due_completes_after_two_ticks() {
        // Window goes to background, idle is not due, should flush after 2 ticks (10-20s)

        let mut cache = FakeCache::default();
        let mut pending = 2;

        // Tick 1: decrement but no flush
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush on tick 1");
        assert_eq!(cache.flushes, 0);
        assert_eq!(pending, 1);

        // Tick 2: should trigger hide flush
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "hide flush was performed");
        assert_eq!(cache.flushes, 1, "flush should trigger on second tick");
        assert_eq!(pending, 0);
    }

    #[test]
    fn foreground_to_background_while_idle_due_flushes_on_second_tick() {
        // Window goes to background AND idle is due: idle_flush will run immediately
        // because the else-if checks idle regardless of hide_flush countdown
        let mut cache = FakeCache::default();
        let mut pending = 2;

        // Make sure idle is NOT due initially

        // Tick 1: pending=2, decrements to 1, idle not due, no flush
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush when idle not due");
        assert_eq!(cache.flushes, 0, "no flush when idle not due");
        assert_eq!(pending, 1, "pending still decrements");

        // Now make idle due

        // Tick 2: pending=1, becomes 0, hide_flush fires (takes priority over idle)
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert_eq!(cache.flushes, 1, "hide flush fires on second tick");
        assert_eq!(pending, 0);
        assert!(flushed, "hide flush satisfies idle");
    }

    #[test]
    fn background_to_foreground_resets_pending_to_zero() {
        // Window is in background (pending > 0), then comes back to foreground
        // Make sure idle is NOT due to avoid interference

        let mut cache = FakeCache::default();
        let mut pending = 1; // simulates partial countdown

        // User brings window back to foreground before flush triggers
        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush when returning to foreground");
        assert_eq!(
            cache.flushes, 0,
            "no flush when returning to foreground (idle not due)"
        );
        assert_eq!(pending, 0, "pending should be reset to 0");
    }

    #[test]
    fn multiple_background_entries_only_first_sets_pending() {
        // If window goes background -> foreground -> background quickly,
        // the second background entry should reset pending to 2 again.
        // This test simulates the second background entry.

        let mut cache = FakeCache::default();

        // First background entry
        let mut pending = 2;
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush on first background");
        assert_eq!(pending, 1);

        // User returns to foreground briefly
        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush in foreground");
        assert_eq!(pending, 0);

        // Goes to background again - in real code, this sets pending=2 again
        pending = 2;
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush on countdown restart");
        assert_eq!(cache.flushes, 0, "countdown restarts");
        assert_eq!(pending, 1);

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "hide flush was performed");
        assert_eq!(cache.flushes, 1, "flush after second countdown");
        assert_eq!(pending, 0);
    }

    #[test]
    fn idle_flush_while_in_background_with_no_pending() {
        // Window is in background, pending already consumed (or was 0),
        // but idle becomes due -> idle flush should run

        let mut cache = FakeCache::default();
        let mut pending = 0;

        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);
        assert!(flushed, "idle flush should be satisfied");
        assert_eq!(cache.flushes, 1, "idle flush should run even in background");
        assert_eq!(pending, 0);
    }

    #[test]
    fn no_flush_when_not_idle_and_no_pending() {
        // Window is anywhere, no pending, idle not due -> no flush

        let mut cache = FakeCache::default();
        let mut pending = 0;

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush when conditions not met");
        assert_eq!(cache.flushes, 0, "no flush when neither condition is met");

        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush in foreground");
        assert_eq!(cache.flushes, 0, "still no flush in foreground");
    }

    #[test]
    fn pending_equals_1_triggers_immediately_in_background() {
        // If pending is already 1 (second tick countdown), flush should trigger

        let mut cache = FakeCache::default();
        let mut pending = 1;

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "hide flush was performed");
        assert_eq!(cache.flushes, 1, "pending=1 in background should flush");
        assert_eq!(pending, 0);
    }

    #[test]
    fn pending_greater_than_2_decrements_correctly() {
        // Edge case: if pending is set to 3+ (shouldn't happen normally),
        // it should decrement each tick until reaching 0, then flush

        let mut cache = FakeCache::default();
        let mut pending = 3;

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush during countdown");
        assert_eq!(cache.flushes, 0);
        assert_eq!(pending, 2);

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush during countdown");
        assert_eq!(cache.flushes, 0);
        assert_eq!(pending, 1);

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "hide flush was performed");
        assert_eq!(cache.flushes, 1, "flush triggers when countdown reaches 0");
        assert_eq!(pending, 0);
    }

    #[test]
    fn rapid_foreground_background_transitions() {
        // Test rapid transitions: background -> foreground -> background

        let mut cache = FakeCache::default();

        // Go to background
        let mut pending = 2;
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush on background transition");
        assert_eq!(pending, 1);

        // Immediately back to foreground
        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush when returning to foreground");
        assert_eq!(pending, 0);
        assert_eq!(cache.flushes, 0);

        // Back to background again
        pending = 2;
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush on re-background");
        assert_eq!(pending, 1);

        // Back to foreground before flush
        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush on cancellation");
        assert_eq!(pending, 0);
        assert_eq!(cache.flushes, 0, "should never flush due to cancellations");
    }

    #[test]
    fn idle_triggers_during_hide_countdown() {
        // Window goes to background (pending=2), but before the countdown completes,
        // idle becomes due. The hide flush (on second tick) should preempt idle.
        let mut cache = FakeCache::default();
        let mut pending = 2;

        // First tick: pending=2->1, idle not quite due yet
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "idle not due yet during countdown");
        assert_eq!(cache.flushes, 0);
        assert_eq!(pending, 1);

        // Second tick: pending=1->0, both background and idle are ready,
        // background should preempt
        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);
        assert_eq!(cache.flushes, 1, "single flush (background preempts idle)");
        assert_eq!(pending, 0);
        assert!(flushed, "idle satisfied by hide flush");
    }

    #[test]
    fn consecutive_hide_flushes_require_new_pending() {
        // After a hide flush completes, pending is 0.
        // Idle flush can trigger independently if conditions are met.
        let mut cache = FakeCache::default();
        let mut pending = 1;

        // Set up so we won't hit idle initially

        // First hide flush
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "hide flush was performed");
        assert_eq!(cache.flushes, 1);
        assert_eq!(pending, 0);

        // Ticks continue, but pending is 0 and idle not due -> no more flushes
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush without new trigger");
        assert_eq!(
            cache.flushes, 1,
            "no second flush without new pending or idle"
        );

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "still no idle flush");
        assert_eq!(cache.flushes, 1, "still no flush");

        // Simulate window coming back and going to background again
        pending = 2;
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush during countdown");
        assert_eq!(pending, 1);
        assert_eq!(cache.flushes, 1, "no flush yet, still counting down");

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "second hide flush was performed");
        assert_eq!(cache.flushes, 2, "second hide flush after re-backgrounding");
        assert_eq!(pending, 0);
    }

    #[test]
    fn error_during_hide_flush_retries_until_success() {
        let mut cache = FakeCacheErr;
        let mut pending = 1;

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "failed hide flush must not count as saved");
        assert_eq!(pending, 1, "failed hide flush remains pending");

        let mut cache = FakeCache::default();
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "next tick retries and saves after recovery");
        assert_eq!(cache.flushes, 1);
        assert_eq!(pending, 0, "successful retry consumes pending");
    }

    #[test]
    fn idle_flush_in_foreground_when_due() {
        // Foreground window, idle is due -> idle flush should run

        let mut cache = FakeCache::default();
        let mut pending = 0;

        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);
        assert_eq!(cache.flushes, 1, "idle flush runs in foreground");
        assert!(flushed, "idle window advanced");
    }

    #[test]
    fn no_idle_flush_in_foreground_when_not_due() {
        // Foreground window, idle not due -> no flush

        let mut cache = FakeCache::default();
        let mut pending = 0;

        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush when not due");
        assert_eq!(cache.flushes, 0, "no flush when idle not due");
    }

    #[test]
    fn hide_flush_after_exact_two_ticks_at_ten_seconds_each() {
        // Verifies the exact 10-20 second window: pending=2 means first tick at ~10s, second at ~20s

        let mut cache = FakeCache::default();
        let mut pending = 2;

        // Tick at ~10s
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush at ~10s mark");
        assert_eq!(cache.flushes, 0, "no flush at ~10s mark");
        assert_eq!(pending, 1);

        // Tick at ~20s
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "hide flush was performed at ~20s");
        assert_eq!(cache.flushes, 1, "flush at ~20s mark");
        assert_eq!(pending, 0);
    }

    #[test]
    fn idle_flush_error_preserves_idle_window() {
        let mut cache = FakeCacheErr;
        let mut pending = 0;

        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);

        assert!(!flushed, "failed save must preserve the idle deadline");
    }

    #[test]
    fn idle_at_exact_5_minute_boundary() {
        let mut cache = FakeCache::default();
        let mut pending = 0;

        // At exactly 5 minutes, should trigger
        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);
        assert!(flushed, "idle flush should be satisfied at exact boundary");
        assert_eq!(cache.flushes, 1, "flush should trigger at exact boundary");
    }

    #[test]
    fn idle_just_under_5_minute_boundary() {
        // Test behavior when idle is 1 second under threshold

        let mut cache = FakeCache::default();
        let mut pending = 0;

        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush when under threshold");
        assert_eq!(cache.flushes, 0, "no flush when under threshold");
    }

    #[test]
    fn background_to_foreground_with_idle_due() {
        // Window returns to foreground while idle is due and pending>0
        // Pending gets consumed, and idle flush triggers

        let mut cache = FakeCache::default();
        let mut pending = 1; // was in background countdown

        // Returns to foreground with idle due
        // Since we're in foreground, hide_flush won't fire (even if pending becomes 0)
        // But idle_flush will fire because it's due
        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);
        assert!(flushed, "idle flush should be satisfied");
        assert_eq!(cache.flushes, 1, "idle flush should trigger in foreground");
        assert_eq!(pending, 0, "pending should be consumed");
    }

    #[test]
    fn pending_u8_max_decrements_correctly() {
        // Edge case: pending at u8::MAX should decrement without overflow

        let mut cache = FakeCache::default();
        let mut pending = u8::MAX;

        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush during MAX countdown");
        assert_eq!(pending, u8::MAX - 1, "should decrement from MAX");
        assert_eq!(cache.flushes, 0, "no flush at MAX");

        // Continue decrementing
        for _ in 0..(u8::MAX - 2) {
            let _flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        }
        assert_eq!(pending, 1, "should reach 1");

        // Final tick triggers flush
        let flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
        assert!(flushed, "hide flush was performed");
        assert_eq!(cache.flushes, 1, "flush when reaching 0");
        assert_eq!(pending, 0);
    }

    #[test]
    fn consecutive_idle_flushes() {
        // Multiple idle flushes should each reset the idle window
        // Note: Each set_idle_over_5m call sets the timestamp independently
        let mut cache = FakeCache::default();
        let mut pending = 0;

        // First idle flush

        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);
        assert!(flushed, "first idle flush should be satisfied");
        assert_eq!(cache.flushes, 1, "first idle flush");

        // Second idle flush - set idle again

        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);
        assert!(flushed, "second idle flush should be satisfied");
        assert_eq!(cache.flushes, 2, "second idle flush");

        // Third idle flush

        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);
        assert!(flushed, "third idle flush should be satisfied");
        assert_eq!(cache.flushes, 3, "third idle flush");
    }

    #[test]
    fn hide_flush_pending_1_with_idle_also_due() {
        // Both hide (pending=1) and idle are ready, hide should win

        let mut cache = FakeCache::default();
        let mut pending = 1;

        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);
        assert!(flushed, "hide flush satisfies idle when both ready");
        assert_eq!(cache.flushes, 1, "single flush (hide wins)");
        assert_eq!(pending, 0);
        assert!(flushed, "idle satisfied by hide flush");
    }

    #[test]
    fn idle_flush_error_then_success() {
        // Idle flush fails, then succeeds on retry

        let mut cache_err = FakeCacheErr;
        let mut pending = 0;

        let flushed = start_flush_checks(|| true, || true, &mut cache_err, &mut pending);
        assert!(!flushed, "failed idle save must preserve its deadline");

        // Time passes again

        let mut cache_ok = FakeCache::default();
        let flushed = start_flush_checks(|| true, || true, &mut cache_ok, &mut pending);
        assert!(flushed, "idle flush satisfied on success");
        assert_eq!(cache_ok.flushes, 1, "should succeed on retry");
    }

    #[test]
    fn pending_zero_stays_zero() {
        // Calling with pending=0 should keep it at 0

        let mut cache = FakeCache::default();
        let mut pending = 0;

        for _ in 0..10 {
            let _flushed = start_flush_checks(|| false, || false, &mut cache, &mut pending);
            assert_eq!(pending, 0, "pending should stay at 0");
        }
        assert_eq!(
            cache.flushes, 0,
            "no flushes when pending=0 and idle not due"
        );
    }

    #[test]
    fn foreground_with_pending_and_idle_both_not_ready() {
        // Foreground, pending=5, idle not due -> pending decrements, no flush

        let mut cache = FakeCache::default();
        let mut pending = 5;

        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush in foreground");
        assert_eq!(pending, 4, "pending decrements in foreground");
        assert_eq!(cache.flushes, 0, "no flush");

        let flushed = start_flush_checks(|| true, || false, &mut cache, &mut pending);
        assert!(!flushed, "no idle flush in foreground");
        assert_eq!(pending, 3);
        assert_eq!(cache.flushes, 0);
    }

    #[test]
    fn hide_flush_success_then_immediate_idle_due() {
        // Hide flush completes, then immediately idle becomes due (edge case)
        let mut cache = FakeCache::default();
        let mut pending = 1;

        // Hide flush triggers
        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);
        assert!(flushed, "hide flush satisfies idle");
        assert_eq!(cache.flushes, 1);
        assert_eq!(pending, 0);

        // Now idle becomes due

        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);
        assert!(flushed, "idle flush should be satisfied after hide");
        assert_eq!(cache.flushes, 2, "idle flush should trigger after hide");
    }

    #[test]
    fn alternating_hide_and_idle_flushes() {
        // Alternate between hide and idle flushes
        let mut cache = FakeCache::default();

        // Hide flush (make sure idle is not due)

        let mut pending = 1;
        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);
        assert!(flushed, "hide flush satisfies idle");
        assert_eq!(cache.flushes, 1, "first hide flush");
        assert_eq!(pending, 0);

        // Idle flush (set idle to be due)
        pending = 0;

        let flushed = start_flush_checks(|| true, || true, &mut cache, &mut pending);
        assert!(flushed, "idle flush should be satisfied");
        assert_eq!(cache.flushes, 2, "first idle flush");

        // Hide flush again (reset idle to not due)

        pending = 1;
        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);
        assert!(flushed, "hide flush satisfies idle");
        assert_eq!(cache.flushes, 3, "second hide flush");
        assert_eq!(pending, 0);

        // Idle again (set idle to be due)
        pending = 0;

        let flushed = start_flush_checks(|| false, || true, &mut cache, &mut pending);
        assert!(flushed, "idle flush should be satisfied");
        assert_eq!(cache.flushes, 4, "second idle flush");
    }
}
