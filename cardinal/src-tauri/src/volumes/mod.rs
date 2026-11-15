//! Independently checkpoint each external volume. A snapshot is named for the
//! FSEvents journal it belongs to and contains a volume-relative tree.
mod mounts;
pub(crate) mod query;
#[cfg(test)]
mod tests;

use crate::lifecycle::APP_QUIT;
use anyhow::{Context, Result, ensure};
use cardinal_sdk::{EventFlag, EventStream, current_timestamp, last_event_id_before_time};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use fswalk::{WalkData, walk_it_without_root_chain};
use mounts::{Mount, discover, uuid};
use parking_lot::Mutex;
use query::Index;
use search_cache::{EventBuffer, EventObservation, SearchCache, read_snapshot_header};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsStr,
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tauri::Emitter;
use tracing::warn;

type Notify = Arc<dyn Fn() + Send + Sync>;
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VolumeInfo {
    id: String,
    path: PathBuf,
    format: String,
    online: bool,
    indexing: bool,
    reusable: bool,
    revision: u64,
    count: Option<usize>,
    error: Option<String>,
}

struct Meta {
    info: VolumeInfo,
    mount: Option<Mount>,
    snapshot: Option<PathBuf>,
}

struct Volume {
    meta: Mutex<Meta>,
    index: Mutex<Option<Index>>,
    generation: AtomicU64,
    rebuild: AtomicBool,
    last_query: Mutex<Instant>,
    can_unload: AtomicBool,
}
impl Volume {
    fn new(id: &str, path: PathBuf, format: &str, db: Option<PathBuf>) -> Self {
        Self {
            meta: Mutex::new(Meta {
                info: VolumeInfo {
                    id: id.into(),
                    path,
                    format: format.into(),
                    online: false,
                    indexing: false,
                    reusable: uuid(id),
                    revision: 0,
                    count: None,
                    error: None,
                },
                mount: None,
                snapshot: db,
            }),
            index: Mutex::new(None),
            generation: AtomicU64::new(0),
            rebuild: AtomicBool::new(false),
            last_query: Mutex::new(Instant::now()),
            can_unload: AtomicBool::new(true),
        }
    }

    // Lock order: index then meta. UI discovery only needs meta.
    fn load_snapshot(&self, data: &mut Option<Index>) -> Result<()> {
        if data.is_none() {
            let (snapshot, path) = {
                let meta = self.meta.lock();
                (meta.snapshot.clone(), meta.info.path.clone())
            };
            if let Some(file) = snapshot {
                let header = read_snapshot_header(&file)?;
                let mut cache = SearchCache::try_read_persistent_cache(
                    &header.path,
                    &file,
                    &header.ignore_paths,
                    &[],
                    &APP_QUIT,
                )?;
                cache.restore_volume_root(&path);
                self.meta.lock().info.count = Some(cache.get_total_files());
                *data = Some(Index::new(cache));
            }
        }
        Ok(())
    }

    fn current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }
}

#[derive(Clone, Default)]
pub(crate) struct Volumes(Arc<Mutex<BTreeMap<String, Arc<Volume>>>>);
impl Volumes {
    fn get(&self, id: &str) -> Result<Arc<Volume>> {
        self.0.lock().get(id).cloned().context("Unknown volume")
    }
    fn list(&self) -> Vec<VolumeInfo> {
        let mut v: Vec<_> = self
            .0
            .lock()
            .values()
            .map(|s| s.meta.lock().info.clone())
            .collect();
        v.sort_by(|a, b| b.online.cmp(&a.online).then_with(|| a.path.cmp(&b.path)));
        v
    }

    fn restore(&self, dir: &Path) -> Result<()> {
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(dir)?;
        for entry in fs::read_dir(dir)?.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if !uuid(id.strip_prefix("snapshot-").unwrap_or(&id)) || !entry.file_type()?.is_dir() {
                continue;
            }
            let mut files: Vec<_> = fs::read_dir(entry.path())?
                .flatten()
                .filter(|p| {
                    p.file_type().is_ok_and(|t| t.is_file())
                        && p.path().extension() == Some(OsStr::new("db"))
                })
                .collect();
            files.sort_by_cached_key(|p| {
                std::cmp::Reverse(p.metadata().ok().and_then(|m| m.modified().ok()))
            });
            for file in files {
                let path = file.path();
                let Some((format, epoch)) = path
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .and_then(|s| s.split_once('.'))
                else {
                    continue;
                };
                if (epoch != "none" && !uuid(epoch))
                    || !format
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    continue;
                }
                if let Ok(header) = read_snapshot_header(&path) {
                    self.0.lock().insert(
                        id.clone(),
                        Arc::new(Volume::new(&id, header.path, format, Some(path.clone()))),
                    );
                    break;
                }
            }
        }
        Ok(())
    }

    fn monitor(&self, dir: PathBuf, wake: Receiver<()>, notify: Notify) {
        if let Err(e) = self.restore(&dir) {
            warn!("Reading removable volume caches: {e:#}");
        }
        notify();
        let mut workers: Vec<thread::JoinHandle<()>> = vec![];
        while !APP_QUIT.load(Ordering::Relaxed) {
            if let Some(mounts) = discover() {
                workers.extend(self.reconcile(&dir, mounts, &notify));
            }
            workers.retain(|w| !w.is_finished());
            // NSWorkspace normally wakes us immediately. Reconcile after sleep or a missed notification.
            let _ = wake.recv_timeout(Duration::from_secs(30));
        }
        for handle in workers {
            let _ = handle.join();
        }
    }

    fn reconcile(
        &self,
        dir: &Path,
        mounts: Vec<Mount>,
        notify: &Notify,
    ) -> Vec<thread::JoinHandle<()>> {
        let mut sources = self.0.lock();
        let mut present = HashMap::new();
        for m in mounts {
            let id = if m.persistent {
                m.id.clone()
            } else {
                sources
                    .iter()
                    .find_map(|(key, src)| {
                        let meta = src.meta.lock();
                        (meta.mount.as_ref() == Some(&m)).then(|| key.clone())
                    })
                    .unwrap_or_else(mounts::separate_snapshot_id)
            };
            sources
                .entry(id.clone())
                .or_insert_with(|| Arc::new(Volume::new(&id, m.path.clone(), &m.format, None)));
            present.insert(id, m);
        }
        let mut changed = false;
        let mut workers = Vec::new();
        sources.retain(|id, source| {
            let next = present.remove(id);
            let mut meta = source.meta.lock();
            let fresh = source.rebuild.swap(false, Ordering::AcqRel);
            if meta.mount == next && !fresh {
                if next.is_none()
                    && source.can_unload.load(Ordering::Acquire)
                    && source.last_query.lock().elapsed() > Duration::from_secs(90)
                    && let Some(mut index) = source.index.try_lock()
                {
                    *index = None;
                }
                return true;
            }
            let generation = source.generation.fetch_add(1, Ordering::AcqRel) + 1;
            meta.mount = next.clone();
            meta.info.online = next.is_some();
            meta.info.indexing = next.is_some();
            meta.info.error = None;
            meta.info.revision += 1;
            changed = true;
            if let Some(mount) = next {
                source.can_unload.store(false, Ordering::Release);
                meta.info.path = mount.path.clone();
                meta.info.format = mount.format.clone();
                let dir = dir.to_owned();
                let source = Arc::clone(source);
                let notify = Arc::clone(notify);
                workers.push(thread::spawn(move || {
                    run_mount(&source, &mount, generation, &dir, notify, fresh)
                }));
                true
            } else {
                // No completed first scan means there is no offline cache to offer.
                meta.snapshot.is_some() || meta.info.count.is_some()
            }
        });
        drop(sources);
        if changed {
            notify();
        }
        workers
    }
}

pub(crate) fn mount_notifications() -> Receiver<()> {
    mounts::subscribe()
}
pub(crate) fn stop_monitor() {
    mounts::wake();
}
pub(crate) fn monitor(store: Volumes, dir: PathBuf, wake: Receiver<()>, app: tauri::AppHandle) {
    store.monitor(
        dir,
        wake,
        Arc::new(move || {
            let _ = app.emit("volumes_changed", ());
        }),
    );
}

#[tauri::command]
pub(crate) fn list_volumes(volumes: tauri::State<'_, Volumes>) -> Vec<VolumeInfo> {
    volumes.list()
}

#[tauri::command]
pub(crate) fn rescan_volume(id: String, volumes: tauri::State<'_, Volumes>) -> Result<(), String> {
    let source = volumes.get(&id).map_err(|e| e.to_string())?;
    if source.meta.lock().mount.is_none() {
        return Err("Volume is offline".into());
    }
    source.rebuild.store(true, Ordering::Release);
    mounts::wake();
    Ok(())
}

#[tauri::command(async)]
pub(crate) fn open_volume_path(
    id: String,
    path: PathBuf,
    reveal: bool,
    volumes: tauri::State<'_, Volumes>,
) -> Result<(), String> {
    let source = volumes.get(&id).map_err(|e| e.to_string())?;
    let mount = source
        .meta
        .lock()
        .mount
        .clone()
        .ok_or("Volume is offline")?;
    if !mount.verified_path(&path) {
        return Err("Volume is offline or the mount has changed".into());
    }
    let mut cmd = std::process::Command::new("/usr/bin/open");
    if reveal {
        cmd.arg("-R");
    }
    cmd.arg(&path).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

const OMIT: [&str; 5] = [
    ".fseventsd",
    ".Spotlight-V100",
    ".Trashes",
    ".TemporaryItems",
    ".DocumentRevisions-V100",
];

fn exclusions(mount: &Mount) -> Vec<PathBuf> {
    OMIT.iter()
        .map(|n| mount.path.join(n))
        .chain(mount.nested.iter().cloned())
        .collect()
}

fn build(
    mount: &Mount,
    cursor: u64,
    stopped: impl Fn() -> bool + Sync + Send,
) -> Result<SearchCache> {
    // walk_it can represent an unreadable root as an empty node. Preserve a
    // known-good snapshot if the mount is disappearing or access was denied.
    fs::read_dir(&mount.path).context("Cannot read volume root")?;
    let ignores = exclusions(mount);
    let fast = if mount.format == "hfs" && mount.nested.is_empty() {
        match fswalk::walk_hfs_catalog(&mount.path, &stopped) {
            Ok(tree) => Some(tree),
            Err(e) => {
                tracing::debug!("HFS+ catalog fallback: {e}");
                None
            }
        }
    } else {
        None
    };
    let mut tree = fast
        .unwrap_or_else(|| {
            walk_it_without_root_chain(&WalkData::new(&mount.path, &ignores, &[], true, &stopped))
        })
        .context("Volume scan interrupted")?;
    ensure!(
        tree.metadata.is_some() && !stopped() && mount.present(),
        "Volume disconnected during scan"
    );
    tree.children
        .retain(|node| !OMIT.contains(&node.name.as_ref()));
    Ok(SearchCache::from_volume_tree(
        &mount.path,
        &tree,
        ignores,
        cursor,
        &APP_QUIT,
    ))
}

fn checkpoint(source: &Volume, index: &mut Index, db: &Path) -> Result<()> {
    index.cache.flush_snapshot_to_file(db)?;
    let old = source.meta.lock().snapshot.replace(db.to_owned());
    if let Some(old) = old
        && old != db
    {
        let _ = fs::remove_file(old);
    }
    Ok(())
}

fn run_mount(
    source: &Volume,
    mount: &Mount,
    generation: u64,
    dir: &Path,
    notify: Notify,
    mut fresh: bool,
) {
    while source.current(generation) && !APP_QUIT.load(Ordering::Relaxed) {
        match watch(source, mount, generation, dir, &notify, fresh) {
            Ok(false) => {
                let meta = source.meta.lock();
                if source.current(generation)
                    || (source.current(generation + 1) && meta.mount.is_none())
                {
                    source
                        .can_unload
                        .store(meta.snapshot.is_some(), Ordering::Release);
                }
                break;
            }
            Ok(true) => fresh = true,
            Err(e) => {
                tracing::debug!("Volume worker: {e:#}");
                if !source.current(generation) || !mount.present() {
                    break;
                }
                {
                    let mut meta = source.meta.lock();
                    meta.info.error = Some(e.to_string());
                    meta.info.indexing = false;
                }
                notify();
                // Retry recoverable media/permission failures while the drive remains attached.
                for _ in 0..30 {
                    if !source.current(generation) || APP_QUIT.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(250));
                }
                fresh = true;
            }
        }
    }
}

fn watch(
    source: &Volume,
    mount: &Mount,
    generation: u64,
    dir: &Path,
    notify: &Notify,
    fresh: bool,
) -> Result<bool> {
    let stopped = || !source.current(generation) || APP_QUIT.load(Ordering::Relaxed);
    let db = dir.join(&source.meta.lock().info.id).join(format!(
        "{}.{}.db",
        mount.format,
        mount.journal.as_deref().unwrap_or("none")
    ));
    let tip = last_event_id_before_time(mount.dev, current_timestamp());
    let saved = {
        let mut data = source.index.lock();
        // A damaged volume snapshot can still be replaced by a complete new scan.
        if let Err(e) = source.load_snapshot(&mut data) {
            warn!("Load volume cache: {e:#}");
        }
        if stopped() {
            return Ok(false);
        }
        let mut meta = source.meta.lock();
        meta.info.indexing = true;
        meta.info.error = None;
        if let Some(index) = data.as_mut() {
            index.cache.restore_volume_root(&mount.path);
            index.changed();
        }
        // Foreign FAT/NTFS writers need not update Apple's journal. Offer the
        // cache immediately and reconcile their directory tree on attachment.
        data.as_mut()
            .filter(|i| i.cache.ignore_paths().as_ref() == exclusions(mount))
            .map(|i| i.cache.last_event_id())
            .filter(|&cursor| {
                !fresh
                    && matches!(mount.format.as_str(), "hfs" | "apfs")
                    && mount.journal.is_some()
                    && meta.snapshot.as_ref() == Some(&db)
                    && cursor != 0
                    && cursor <= tip
            })
    };
    notify();
    let device_stream = mount.journal.as_ref().and_then(|_| {
        EventStream::watch_volume(mount.dev, mount.path.clone(), saved.unwrap_or(u64::MAX))
    });
    // A drive without a persistent device stream can still be searched and watched live.
    let live_only = device_stream.is_none();
    let stream =
        device_stream.or_else(|| mount.path.to_str().and_then(EventStream::watch_live_path));
    let idle = crossbeam_channel::never();
    let events = stream.as_ref().map(|(rx, _guard)| rx).unwrap_or(&idle);
    let db = if live_only {
        db.with_file_name(format!("{}.none.db", mount.format))
    } else {
        db
    };
    let mut dirty = false;
    if saved.is_none() || live_only {
        let cache = build(mount, tip, stopped)?;
        // Inspect the actual identity again before publishing after a potentially long scan.
        ensure!(
            !stopped() && mount.verified_path(&mount.path),
            "Mount changed during indexing"
        );
        let mut data = source.index.lock();
        if stopped() {
            return Ok(false);
        }
        *data = Some(Index::new(cache));
        checkpoint(source, data.as_mut().unwrap(), &db)?;
    } else {
        // Persist a renamed mount point even if its event journal has no new file events.
        dirty = read_snapshot_header(&db).is_ok_and(|h| h.path != mount.path);
    }
    {
        let data = source.index.lock();
        let mut meta = source.meta.lock();
        if stopped() {
            return Ok(false);
        }
        meta.info.count = data.as_ref().map(|i| i.cache.get_total_files());
        meta.info.indexing = false;
        meta.info.revision += 1;
    }
    notify();

    let mut last_checkpoint = Instant::now();
    let mut buffered = EventBuffer::default();
    // Ejection can deliver a root-rescan flag or end the event stream before
    // the mount notification. Every exit must save already reconciled changes.
    let outcome = loop {
        if stopped() {
            break Ok(false);
        }
        // Some third-party drivers have no notification stream. A read-only
        // mount needs no polling. Writable media gets a background reconciliation.
        if stream.is_none()
            && !mount.read_only
            && last_checkpoint.elapsed() >= Duration::from_secs(60)
        {
            break Ok(true);
        }
        let now = Instant::now();
        let timeout = Duration::from_millis(250);
        let timeout = buffered.deadline().map_or(timeout, |due| {
            timeout.min(due.saturating_duration_since(now))
        });
        let mut batch: Vec<_> = match events.recv_timeout(timeout) {
            Ok(batch) => batch
                .into_iter()
                .chain(events.try_iter().take(31).flatten())
                .filter(|e| {
                    if e.flag.contains(EventFlag::HistoryDone)
                        || (e.path == mount.path && e.flag == EventFlag::Mount)
                    {
                        return false;
                    }
                    let relative = e.path.strip_prefix(&mount.path).unwrap_or(&e.path);
                    !relative
                        .iter()
                        .next()
                        .is_some_and(|c| OMIT.iter().any(|n| c == OsStr::new(n)))
                })
                .collect(),
            Err(RecvTimeoutError::Disconnected) => {
                break Err(anyhow::anyhow!("Volume event stream ended"));
            }
            Err(RecvTimeoutError::Timeout) => vec![],
        };
        let now = Instant::now();
        if !batch.is_empty() || buffered.deadline().is_some_and(|due| now >= due) {
            // Never save host stream event IDs as device journal cursors.
            if live_only {
                for e in &mut batch {
                    e.id = 0;
                }
            }
            let plan = {
                let mut data = source.index.lock();
                let index = data.as_mut().context("Volume index unavailable")?;
                match buffered.prepare(&mut index.cache, batch, now) {
                    Ok(Some((plan, _))) => plan,
                    Ok(None) => continue,
                    Err(_) => break Ok(!stopped() && mount.present()),
                }
            };
            if !mount.present() || stopped() {
                break Ok(false);
            }
            // Reconcile off the UI lock, and commit the batch only while this volume still owns the mount.
            let mut changes = Vec::new();
            plan.read(stopped, |c| changes.push(c));
            if stopped() || !mount.present() {
                break Ok(false);
            }
            let visible = changes
                .iter()
                .any(|c| !matches!(c, EventObservation::Done(_)));
            {
                let mut data = source.index.lock();
                if stopped() {
                    break Ok(false);
                }
                let index = data.as_mut().context("Volume index unavailable")?;
                for change in changes {
                    index.cache.apply_observation(change);
                }
                dirty = true;
                if visible {
                    index.changed();
                    let mut meta = source.meta.lock();
                    meta.info.count = Some(index.cache.get_total_files());
                    meta.info.revision += 1;
                }
            }
            if visible {
                notify();
            }
        }
        // Writing a full volume snapshot compresses the entire index. Save on
        // detach/quit as well, so a sustained stream does not force one every 10s.
        if dirty && last_checkpoint.elapsed() >= Duration::from_secs(300) {
            let mut data = source.index.lock();
            if stopped() {
                break Ok(false);
            }
            if let Some(index) = data.as_mut() {
                checkpoint(source, index, &db)?;
            }
            last_checkpoint = Instant::now();
            dirty = false;
        }
    };
    if let Some(due) = buffered.deadline()
        && APP_QUIT.load(Ordering::Relaxed)
        && source.current(generation)
        && mount.verified_path(&mount.path)
    {
        let mut data = source.index.lock();
        if let Some(index) = data.as_mut()
            && let Ok(Some((plan, _))) = buffered.prepare(&mut index.cache, vec![], due)
        {
            let mut changes = Vec::new();
            plan.read(|| !source.current(generation), |c| changes.push(c));
            if source.current(generation) && mount.verified_path(&mount.path) {
                for change in changes {
                    index.cache.apply_observation(change);
                }
                dirty = true;
            }
        }
    }
    // A later attachment has its own worker. Never overwrite its generation.
    if dirty {
        let mut data = source.index.lock();
        let meta = source.meta.lock();
        let save =
            source.current(generation) || (source.current(generation + 1) && meta.mount.is_none());
        drop(meta);
        if save && let Some(index) = data.as_mut() {
            checkpoint(source, index, &db)?;
        }
    }
    outcome
}
