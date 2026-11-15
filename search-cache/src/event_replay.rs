//! Reconcile an FSEvents delivery against the on-disk state. Directory reads run
//! outside the index writer. Each message can be applied between UI requests.
use super::*;
use fswalk::{DirectoryMetadataEntry, NodeFileType, read_directory_metadata};
use std::{collections::HashMap, sync::Arc, time::Duration};

const SMALL_FILE_BYTES: i64 = 4096;
const SMALL_FILE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_DEFERRED_PATHS: usize = 4096;

/// Per event stream. Keep the durable cursor behind any work still only in memory.
#[derive(Default)]
pub struct EventBuffer {
    pending: HashMap<PathBuf, (EventFlag, u64)>,
    deadline: Option<Instant>,
    received_through: Option<u64>,
}

impl EventBuffer {
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn prepare(
        &mut self,
        cache: &mut SearchCache,
        incoming: Vec<FsEvent>,
        now: Instant,
    ) -> Result<Option<(EventPlan, Vec<FsEvent>)>, HandleFSEError> {
        let mut ready = Vec::new();
        let mut flush = self.deadline.is_some_and(|deadline| now >= deadline);
        for mut event in incoming {
            self.received_through = self.received_through.max(Some(event.id));
            if event.flag.contains(EventFlag::HistoryDone) {
                flush = true;
            }
            if can_delay_write(event.flag) {
                if let Some((flags, id)) = self.pending.get_mut(&event.path) {
                    *flags |= event.flag;
                    *id = (*id).max(event.id);
                    continue;
                }
                if self.pending.len() < MAX_DEFERRED_PATHS
                    && cache.is_small_uncommon_file(&event.path)
                {
                    self.deadline.get_or_insert(now + SMALL_FILE_INTERVAL);
                    self.pending.insert(event.path, (event.flag, event.id));
                    flush |= self.pending.len() == MAX_DEFERRED_PATHS;
                    continue;
                }
            }
            // A rename/removal of a queued path must be visible immediately.
            if let Some((flags, _)) = self.pending.remove(&event.path) {
                event.flag |= flags;
            }
            ready.push(event);
        }
        if flush {
            ready.extend(
                self.pending
                    .drain()
                    .map(|(path, (flag, id))| FsEvent { path, flag, id }),
            );
        }
        let checkpoint = if self.pending.is_empty() {
            self.deadline = None;
            self.received_through.take()
        } else {
            None
        };
        if ready.is_empty() {
            return Ok(None);
        }
        let mut plan = cache.plan_events(&ready)?;
        plan.last_event = checkpoint;
        Ok(Some((plan, ready)))
    }
}

fn can_delay_write(flag: EventFlag) -> bool {
    let writes = EventFlag::ItemModified
        | EventFlag::ItemInodeMetaMod
        | EventFlag::ItemXattrMod
        | EventFlag::ItemChangeOwner
        | EventFlag::ItemFinderInfoMod;
    flag.contains(EventFlag::ItemIsFile)
        && flag.intersects(writes)
        && (flag - (writes | EventFlag::ItemIsFile | EventFlag::OwnEvent)).is_empty()
}

pub struct EventPlan {
    paths: Vec<(PathBuf, bool)>,
    ignore: Box<[PathBuf]>,
    include: Box<[PathBuf]>,
    last_event: Option<u64>,
    // User-entered restoration roots may name a path underneath a symlink.
    // Only follow the same directories an ordinary walk could have entered.
    restore_root: Option<PathBuf>,
}

pub struct EventReplay {
    rx: crossbeam_channel::Receiver<EventObservation>,
    cancelled: Arc<AtomicBool>,
}

pub enum EventObservation {
    Node(PathBuf, NodeMetadata),
    Missing(PathBuf),
    Children(PathBuf, Vec<DirectoryMetadataEntry>),
    Done(Option<u64>),
}

impl Drop for EventReplay {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl EventReplay {
    pub fn observations(&self) -> &crossbeam_channel::Receiver<EventObservation> {
        &self.rx
    }
}

impl EventPlan {
    pub(super) fn restore_paths(cache: &SearchCache, paths: &[PathBuf]) -> Self {
        Self {
            paths: paths.iter().cloned().map(|path| (path, true)).collect(),
            ignore: cache.ignore_paths(),
            include: cache.include_paths(),
            last_event: None,
            restore_root: Some(cache.file_nodes.path().to_owned()),
        }
    }

    pub fn start(self, stop: &'static AtomicBool) -> EventReplay {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = Arc::clone(&cancelled);
        rayon::spawn(move || {
            self.read(
                || stop.load(Ordering::Relaxed) || token.load(Ordering::Relaxed),
                |change| {
                    let _ = tx.send(change);
                },
            )
        });
        EventReplay { rx, cancelled }
    }

    pub fn read(self, cancelled: impl Fn() -> bool, mut emit: impl FnMut(EventObservation)) {
        for (path, recursive) in self.paths {
            if cancelled() {
                return;
            }
            if let Some(root) = &self.restore_root {
                let Ok(relative) = path.strip_prefix(root) else {
                    continue;
                };
                let mut parent = root.clone();
                if relative.iter().any(|name| {
                    let directory = std::fs::symlink_metadata(&parent).is_ok_and(|m| m.is_dir());
                    parent.push(name);
                    !directory
                }) {
                    continue;
                }
            }
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => {
                    let is_dir = meta.is_dir();
                    emit(EventObservation::Node(path.clone(), meta.into()));
                    if !is_dir || !recursive {
                        continue;
                    }
                }
                Err(error) => {
                    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) {
                        emit(EventObservation::Missing(path));
                    }
                    continue;
                }
            }
            let mut directories = vec![path];
            while let Some(dir) = directories.pop() {
                if cancelled() {
                    return;
                }
                let Ok(Some(mut children)) = read_directory_metadata(&dir, &cancelled) else {
                    // An unreadable directory is not evidence that existing children vanished.
                    continue;
                };
                children.retain(|child| {
                    !should_ignore_path(&dir.join(&child.name), &self.ignore, &self.include)
                });
                for child in &children {
                    if child
                        .metadata
                        .is_some_and(|m| m.r#type == NodeFileType::Dir)
                    {
                        directories.push(dir.join(&child.name));
                    }
                }
                emit(EventObservation::Children(dir, children));
            }
        }
        if !cancelled() {
            emit(EventObservation::Done(self.last_event));
        }
    }
}

impl SearchCache {
    fn is_small_uncommon_file(&self, path: &Path) -> bool {
        if path
            .extension()
            .is_some_and(crate::query::common_file_extension)
        {
            return false;
        }
        self.node_index_for_path(path)
            .and_then(|id| self.file_nodes[id].metadata.as_ref())
            .is_some_and(|m| {
                m.r#type() == NodeFileType::File && (0..SMALL_FILE_BYTES).contains(&m.size())
            })
    }

    pub fn plan_events(&mut self, events: &[FsEvent]) -> Result<EventPlan, HandleFSEError> {
        if events
            .iter()
            .any(|e| e.should_rescan(self.file_nodes.path()))
        {
            self.rescan_count = self.rescan_count.saturating_add(1);
            return Err(HandleFSEError::Rescan);
        }
        let mut flags = HashMap::new();
        for e in events {
            if e.flag.scan_type() != ScanType::Nop
                && e.path.starts_with(self.file_nodes.path())
                && !self.should_ignore(&e.path)
            {
                *flags.entry(&e.path).or_insert(EventFlag::None) |= e.flag;
            }
        }
        let structural = EventFlag::MustScanSubDirs
            | EventFlag::ItemCreated
            | EventFlag::ItemRemoved
            | EventFlag::ItemRenamed
            | EventFlag::Mount
            | EventFlag::Unmount
            | EventFlag::Cloned;
        let mut singles = Vec::new();
        let mut recursive = Vec::new();
        for (path, flag) in flags {
            // With FileEvents, children get independent notifications. A directory's
            // attribute change must not walk or suppress events for its descendants.
            let shallow = flag.contains(EventFlag::ItemIsDir)
                && !flag.intersects(structural)
                && flag.intersects(
                    EventFlag::ItemModified
                        | EventFlag::ItemInodeMetaMod
                        | EventFlag::ItemXattrMod
                        | EventFlag::ItemChangeOwner
                        | EventFlag::ItemFinderInfoMod,
                )
                && self.node_index_for_path(path).is_some_and(|id| {
                    self.file_nodes[id].file_type_hint() == NodeFileType::Dir
                        || !self.file_nodes[id].children.is_empty()
                });
            if shallow {
                singles.push(path.clone());
            } else {
                recursive.push(FsEvent {
                    path: path.clone(),
                    flag,
                    id: 0,
                });
            }
        }
        let roots = scan_paths(recursive);
        let covered: HashSet<_> = roots.iter().cloned().collect();
        let mut paths: Vec<_> = roots.into_iter().map(|path| (path, true)).collect();
        paths.extend(
            singles
                .into_iter()
                .filter(|p| !has_selected_ancestor(p, &covered))
                .map(|path| (path, false)),
        );
        Ok(EventPlan {
            paths,
            restore_root: None,
            ignore: self.ignore_paths(),
            include: self.include_paths(),
            last_event: events.iter().map(|e| e.id).max(),
        })
    }

    pub fn apply_observation(&mut self, change: EventObservation) -> bool {
        match change {
            EventObservation::Done(id) => {
                if let Some(id) = id {
                    self.update_last_event_id(id);
                }
                return true;
            }
            EventObservation::Missing(path) => {
                if let Some(changes) = &mut self.view_changes {
                    changes.record_path(&path);
                }
                self.remove_node_path(&path);
            }
            EventObservation::Node(path, meta) => {
                if let Some(changes) = &mut self.view_changes {
                    changes.record_path(&path);
                }
                if let Some(id) = self.node_index_for_path(&path) {
                    self.update_observed_node(id, Some(meta), true);
                } else if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
                    let parent = self.create_node_chain(parent);
                    let node = SlabNode::new(
                        Some(parent),
                        NAME_POOL.push(&name.to_string_lossy()),
                        SlabNodeMetadataCompact::some(meta),
                    );
                    let id = self.push_node(node);
                    self.file_nodes[parent].add_children(id);
                }
            }
            EventObservation::Children(path, entries) => {
                let Some(id) = self.node_index_for_path(&path) else {
                    return false;
                };
                let mut old: HashMap<_, _> = self.file_nodes[id]
                    .children
                    .iter()
                    .map(|&child| (self.file_nodes[child].name(), child))
                    .collect();
                for entry in entries {
                    let name = entry.name.to_string_lossy();
                    if let Some(child) = old.remove(name.as_ref()) {
                        self.update_observed_node(child, entry.metadata, false);
                    } else {
                        let name = NAME_POOL.push(&name);
                        let metadata = entry
                            .metadata
                            .map(SlabNodeMetadataCompact::some)
                            .unwrap_or_else(SlabNodeMetadataCompact::unaccessible);
                        let child = self.push_node(SlabNode::new(Some(id), name, metadata));
                        self.file_nodes[id].children.push(child);
                    }
                }
                for child in old.into_values() {
                    self.remove_node(child);
                }
            }
        }
        false
    }

    fn update_observed_node(&mut self, id: SlabIndex, meta: Option<NodeMetadata>, explicit: bool) {
        let Some(meta) = meta else { return };
        if meta.r#type != NodeFileType::Dir && !self.file_nodes[id].children.is_empty() {
            let children = std::mem::take(&mut self.file_nodes[id].children);
            for child in children {
                self.remove_node(child);
            }
        }
        let node = &mut self.file_nodes[id];
        let compact = SlabNodeMetadataCompact::some(meta);
        if explicit || node.metadata != compact {
            node.metadata = compact;
            if let Some(changes) = &mut self.view_changes {
                changes.removed_or_modified = true;
                changes.record(id, node.name());
            }
        }
    }
}

#[cfg(test)]
mod buffer_tests {
    use super::*;
    use std::fs;
    use tempdir::TempDir;

    fn event(path: &Path, id: u64, flag: EventFlag) -> FsEvent {
        FsEvent {
            path: path.to_owned(),
            id,
            flag: flag | EventFlag::ItemIsFile,
        }
    }

    fn apply(plan: EventPlan, cache: &mut SearchCache) {
        plan.read(
            || false,
            |change| {
                cache.apply_observation(change);
            },
        );
    }

    #[test]
    fn repeated_small_writes_read_once_by_the_original_deadline_and_hold_the_cursor() {
        let temp = TempDir::new("small-file-events").unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let tiny = root.join("working.tmp");
        let common = root.join("notes.MD");
        fs::write(&tiny, [1]).unwrap();
        fs::write(&common, [2]).unwrap();
        let mut index = SearchCache::walk_fs_with_walk_data(
            &WalkData::new(&root, &[], &[], true, || false),
            &NEVER_STOPPED,
        )
        .unwrap();
        let from = index.last_event_id();
        let now = Instant::now();
        let due = now + SMALL_FILE_INTERVAL;
        let mut buffer = EventBuffer::default();
        let modified = EventFlag::ItemModified;
        for i in 0..4 {
            let at = now + Duration::from_millis(i * 200);
            let work = buffer
                .prepare(&mut index, vec![event(&tiny, from + 1 + i, modified)], at)
                .unwrap();
            assert!(work.is_none());
            assert_eq!(buffer.deadline(), Some(due));
        }
        fs::write(&common, [0; 13]).unwrap();
        let (plan, urgent) = buffer
            .prepare(&mut index, vec![event(&common, from + 5, modified)], now)
            .unwrap()
            .unwrap();
        assert_eq!(urgent.len(), 1);
        apply(plan, &mut index);
        assert_eq!(index.last_event_id(), from);
        assert_eq!(
            index.file_nodes[index.node_index_for_path(&common).unwrap()]
                .metadata
                .as_ref()
                .unwrap()
                .size(),
            13
        );

        // A checkpoint midway through a mixed batch must replay the deferred file on restart.
        let db = temp.path().join("snapshot");
        index.flush_snapshot_to_file(&db).unwrap();
        let mut restored =
            SearchCache::try_read_persistent_cache(&root, &db, &[], &[], &NEVER_STOPPED).unwrap();
        assert_eq!(restored.last_event_id(), from);
        fs::write(&tiny, [0; 4096]).unwrap();
        restored
            .handle_fs_events(vec![
                event(&tiny, from + 4, modified),
                event(&common, from + 5, modified),
            ])
            .unwrap();
        let (plan, delayed) = buffer.prepare(&mut index, vec![], due).unwrap().unwrap();
        assert_eq!(delayed.len(), 1);
        assert_eq!(plan.paths.len(), 1);
        apply(plan, &mut index);
        assert_eq!(index.last_event_id(), from + 5);
        assert_eq!(buffer.deadline(), None);
        let id = index.node_index_for_path(&tiny).unwrap();
        assert_eq!(
            index.file_nodes[id].metadata,
            restored.file_nodes[id].metadata
        );
        assert_eq!(index.file_nodes[id].metadata.as_ref().unwrap().size(), 4096);
        assert!(
            buffer
                .prepare(&mut index, vec![event(&tiny, from + 6, modified)], due)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn eligibility_uses_cached_size_and_preserves_fast_formats_and_structural_events() {
        let dir = TempDir::new("small-file-formats").unwrap();
        let root = dir.path();
        let slow = ["no-extension", "0.tmp", "4095.tmp"];
        let fast = [
            "a.TxT", "a.ZIP", "a.Md", "a.mID", "a.MIDI", "a.pdf", "a.png", "4096.tmp", "4097.tmp",
        ];
        for filename in slow.iter().chain(&fast) {
            let size = filename
                .split('.')
                .next()
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(17);
            fs::write(root.join(filename), vec![0; size]).unwrap();
        }
        let mut index = SearchCache::walk_fs_with_walk_data(
            &WalkData::new(root, &[], &[], true, || false),
            &NEVER_STOPPED,
        )
        .unwrap();
        let mut buffer = EventBuffer::default();
        let now = Instant::now();
        let write = EventFlag::ItemModified;
        let mut id = index.last_event_id();
        for name in slow {
            id += 1;
            assert!(
                buffer
                    .prepare(&mut index, vec![event(&root.join(name), id, write)], now)
                    .unwrap()
                    .is_none()
            );
        }
        for name in fast.into_iter().chain(["new.unknown"]) {
            id += 1;
            let (_, rows) = buffer
                .prepare(&mut index, vec![event(&root.join(name), id, write)], now)
                .unwrap()
                .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].path, root.join(name));
        }
        for flag in [
            EventFlag::ItemCreated,
            EventFlag::ItemRemoved,
            EventFlag::ItemRenamed,
            EventFlag::Cloned,
            EventFlag::MustScanSubDirs,
            EventFlag::IsHardlink,
            EventFlag::ItemIsDir,
            EventFlag::ItemIsSymlink,
        ] {
            assert!(!can_delay_write(flag | write | EventFlag::ItemIsFile));
        }
        let file = root.join("4095.tmp");
        fs::remove_file(&file).unwrap();
        let (plan, immediate) = buffer
            .prepare(
                &mut index,
                vec![event(&file, id + 1, EventFlag::ItemRemoved)],
                now,
            )
            .unwrap()
            .unwrap();
        assert!(immediate[0].flag.contains(EventFlag::ItemRemoved));
        apply(plan, &mut index);
        assert!(index.node_index_for_path(&file).is_none());
        assert!(!buffer.pending.contains_key(&file));
        assert!(
            buffer
                .prepare(
                    &mut index,
                    vec![event(root, id + 2, EventFlag::KernelDropped)],
                    now
                )
                .is_err()
        );
        let mut without_attributes = SearchCache::walk_fs(root);
        assert!(
            EventBuffer::default()
                .prepare(
                    &mut without_attributes,
                    vec![event(&root.join("0.tmp"), 1, write)],
                    now
                )
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_full_buffer_reconciles_all_paths_without_waiting() {
        let root = Path::new("/Volumes/synthetic-test");
        let files = (0..=MAX_DEFERRED_PATHS)
            .map(|i| Node {
                name: format!("{i:05}.tmp").into(),
                children: vec![],
                metadata: Some(NodeMetadata {
                    r#type: NodeFileType::File,
                    size: 1,
                    allocated_size: 4096,
                    ctime: None,
                    mtime: None,
                }),
            })
            .collect();
        let tree = Node {
            name: "synthetic-test".into(),
            children: files,
            metadata: None,
        };
        let mut index = SearchCache::from_volume_tree(root, &tree, vec![], 10, &NEVER_STOPPED);
        let events = tree
            .children
            .iter()
            .enumerate()
            .map(|(i, file)| {
                event(
                    &root.join(file.name.as_ref()),
                    11 + i as u64,
                    EventFlag::ItemModified,
                )
            })
            .collect();
        let mut buffer = EventBuffer::default();
        let (plan, all) = buffer
            .prepare(&mut index, events, Instant::now())
            .unwrap()
            .unwrap();
        assert_eq!(all.len(), MAX_DEFERRED_PATHS + 1);
        assert_eq!(plan.paths.len(), all.len());
        assert_eq!(plan.last_event, Some(11 + MAX_DEFERRED_PATHS as u64));
        assert!(buffer.pending.is_empty());
    }
}
