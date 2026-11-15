//! Reconcile an FSEvents delivery against the on-disk state. Directory reads run
//! outside the index writer. Each message can be applied between UI requests.
use super::*;
use fswalk::{DirectoryMetadataEntry, NodeFileType, read_directory_metadata};
use std::{collections::HashMap, sync::Arc};

pub struct EventPlan {
    paths: Vec<(PathBuf, bool)>,
    ignore: Box<[PathBuf]>,
    include: Box<[PathBuf]>,
    last_event: Option<u64>,
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

    pub(super) fn read(self, cancelled: impl Fn() -> bool, mut emit: impl FnMut(EventObservation)) {
        for (path, recursive) in self.paths {
            if cancelled() {
                return;
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
