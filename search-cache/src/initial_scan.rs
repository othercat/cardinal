//! Publish each directory as it is enumerated. Its children can be read by
//! other workers as soon as the parent's entries have entered the FIFO.
//! The index thread remains the single writer and can answer queries between batches.
use crate::{SearchCache, SlabIndex};
use crossbeam_channel::{Receiver, Sender, bounded};
use fswalk::{NodeFileType, NodeMetadata, read_directory_metadata, should_ignore_path};
use rayon::prelude::*;
use search_cancel::CancellationToken;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

const BATCH_SIZE: usize = 1024;

struct Entry {
    name: Box<str>,
    metadata: Option<NodeMetadata>,
    directory: Option<usize>,
}

struct DirectoryBatch {
    parent: usize,
    entries: Vec<Entry>,
    last: bool,
}

pub struct ScanBatch(Option<DirectoryBatch>);

pub struct InitialScan {
    rx: Receiver<ScanBatch>,
    // Only directories whose final batch has not been applied need a scan ID.
    directories: HashMap<usize, SlabIndex>,
    abandoned: Arc<AtomicBool>,
    stop: &'static AtomicBool,
    token: CancellationToken,
}

impl Drop for InitialScan {
    fn drop(&mut self) {
        self.abandoned.store(true, Ordering::Relaxed);
    }
}

impl InitialScan {
    pub fn start(
        root: PathBuf,
        ignore: Vec<PathBuf>,
        include: Vec<PathBuf>,
        stop: &'static AtomicBool,
        token: CancellationToken,
    ) -> std::io::Result<(SearchCache, Self)> {
        let (cache, watched) = SearchCache::initial_root(&root, &ignore, &include, stop);
        let (tx, rx) = bounded(64);
        let abandoned = Arc::new(AtomicBool::new(false));
        let cancelled = {
            let abandoned = Arc::clone(&abandoned);
            move || {
                stop.load(Ordering::Relaxed)
                    || abandoned.load(Ordering::Relaxed)
                    || token.is_cancelled().is_none()
            }
        };
        std::thread::Builder::new()
            .name("cardinal-enumerate".into())
            .spawn(move || {
                let scanner = DirectoryReader {
                    tx,
                    ignore,
                    include,
                    next_directory: AtomicUsize::new(1),
                    cancelled,
                };
                scanner.visit(&root, 0);
                let _ = scanner.tx.send(ScanBatch(None));
            })?;
        Ok((
            cache,
            Self {
                rx,
                directories: HashMap::from([(0, watched)]),
                abandoned,
                stop,
                token,
            },
        ))
    }

    pub fn batches(&self) -> &Receiver<ScanBatch> {
        &self.rx
    }

    pub fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.token.is_cancelled().is_none()
    }

    pub fn apply(&mut self, cache: &mut SearchCache, batch: ScanBatch) -> bool {
        let Some(batch) = batch.0 else { return true };
        let parent = self.directories[&batch.parent];
        for entry in batch.entries {
            let id = cache.append_scan_node(parent, &entry.name, entry.metadata);
            if let Some(directory) = entry.directory {
                self.directories.insert(directory, id);
            }
        }
        if batch.last {
            self.directories.remove(&batch.parent);
        }
        false
    }
}

struct DirectoryReader<F> {
    tx: Sender<ScanBatch>,
    next_directory: AtomicUsize,
    ignore: Vec<PathBuf>,
    include: Vec<PathBuf>,
    cancelled: F,
}

impl<F: Fn() -> bool + Sync> DirectoryReader<F> {
    fn visit(&self, path: &Path, parent: usize) {
        if (self.cancelled)() {
            return;
        }
        let mut entries = read_directory_metadata(path, &self.cancelled)
            .ok()
            .flatten()
            .unwrap_or_default();
        // Most indexed directories do not intersect an exclude rule.
        if self
            .ignore
            .iter()
            .any(|mask| path.starts_with(mask) || mask.starts_with(path))
        {
            entries.retain(|entry| {
                !should_ignore_path(&path.join(&entry.name), &self.ignore, &self.include)
            });
        }
        let mut children = Vec::new();
        let mut batch = Vec::with_capacity(entries.len().min(BATCH_SIZE));
        for entry in entries {
            if (self.cancelled)() {
                return;
            }
            let directory = entry
                .metadata
                .filter(|meta| meta.r#type == NodeFileType::Dir)
                .map(|_| {
                    let id = self.next_directory.fetch_add(1, Ordering::Relaxed);
                    children.push((path.join(&entry.name), id));
                    id
                });
            let name = entry
                .name
                .into_string()
                .unwrap_or_else(|name| name.to_string_lossy().into_owned());
            batch.push(Entry {
                name: name.into_boxed_str(),
                metadata: entry.metadata,
                directory,
            });
            if batch.len() == BATCH_SIZE {
                let entries = std::mem::replace(&mut batch, Vec::with_capacity(BATCH_SIZE));
                if self
                    .tx
                    .send(ScanBatch(Some(DirectoryBatch {
                        parent,
                        entries,
                        last: false,
                    })))
                    .is_err()
                {
                    return;
                }
            }
        }
        if self
            .tx
            .send(ScanBatch(Some(DirectoryBatch {
                parent,
                entries: batch,
                last: true,
            })))
            .is_err()
        {
            return;
        }
        children
            .into_par_iter()
            .for_each(|(path, id)| self.visit(&path, id));
    }
}
