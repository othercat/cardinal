use crate::{ResultEdit, SearchCache, SlabIndex, live::apply_edits};
use fswalk::NodeFileType;
use rayon::prelude::*;
use search_cancel::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap, HashMap, HashSet},
    sync::Arc,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SortKey {
    Filename,
    FullPath,
    Size,
    Mtime,
    Ctime,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
struct SortEntry {
    index: SlabIndex,
    // Share the directory prefix across siblings, use the already interned basename.
    name: &'static str,
    directory: Arc<str>,
    numeric: i64,
    type_order: u8,
}

pub struct SortSnapshot {
    entries: Vec<SortEntry>,
    key: SortKey,
}

pub struct HydratedRow {
    id: SlabIndex,
    parent: Option<SlabIndex>,
    name: &'static str,
    metadata: crate::SlabNodeMetadataCompact,
}

struct MissingDirectory {
    path: std::path::PathBuf,
    parent: Option<SlabIndex>,
    siblings: usize,
    rows: Vec<(SlabIndex, &'static str)>,
}

pub struct MetadataPlan(Vec<MissingDirectory>);

type MissingByParent = HashMap<Option<SlabIndex>, Vec<(SlabIndex, &'static str)>>;

pub struct MetadataPlanner {
    ids: std::vec::IntoIter<SlabIndex>,
    grouping: MissingByParent,
    parents: Option<<MissingByParent as IntoIterator>::IntoIter>,
    ready: Vec<MissingDirectory>,
}

impl MetadataPlanner {
    pub fn new(ids: &[SlabIndex]) -> Self {
        Self {
            ids: Vec::from(ids).into_iter(),
            grouping: HashMap::new(),
            parents: None,
            ready: Vec::new(),
        }
    }

    /// Yield the single writer between short slices while preparing a legacy
    /// index with millions of missing attributes. No filesystem IO here.
    pub fn advance(&mut self, cache: &SearchCache) -> Option<MetadataPlan> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(6);
        loop {
            if self.ids.len() > 0 {
                for id in self.ids.by_ref().take(2048) {
                    if let Some(node) = cache.file_nodes.get(id)
                        && node.metadata.is_none()
                    {
                        self.grouping.entry(node.parent()).or_default().push((
                            id,
                            if node.parent().is_none() {
                                "/"
                            } else {
                                node.name()
                            },
                        ));
                    }
                }
            } else {
                let parents = self
                    .parents
                    .get_or_insert_with(|| std::mem::take(&mut self.grouping).into_iter());
                for _ in 0..128 {
                    let Some((parent, rows)) = parents.next() else {
                        return Some(MetadataPlan(std::mem::take(&mut self.ready)));
                    };
                    let path = match parent {
                        Some(id) => match cache.node_path(id) {
                            Some(path) => path,
                            None => continue,
                        },
                        None => "/".into(),
                    };
                    let siblings = parent
                        .and_then(|id| cache.file_nodes.get(id))
                        .map_or(1, |n| n.children.len());
                    self.ready.push(MissingDirectory {
                        path,
                        parent,
                        siblings,
                        rows,
                    });
                }
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
        }
    }
}

impl MetadataPlan {
    pub fn run(
        mut self,
        cancelled: impl Fn() -> bool + Sync,
        publish: impl Fn(Vec<HydratedRow>) + Sync,
    ) {
        self.0.par_sort_unstable_by(|a, b| a.path.cmp(&b.path));
        self.0.par_chunks(32).for_each(|directories| {
            let mut updates = Vec::new();
            for group in directories {
                if cancelled() {
                    break;
                }
                let dense = group.rows.len().saturating_mul(4) >= group.siblings;
                let bulk = dense
                    .then(|| {
                        fswalk::read_directory_metadata(&group.path, &cancelled)
                            .ok()
                            .flatten()
                    })
                    .flatten()
                    .map(|entries| {
                        entries
                            .into_iter()
                            .filter_map(|e| Some((e.name, e.metadata?)))
                            .collect::<HashMap<_, _>>()
                    });
                for &(id, name) in &group.rows {
                    if cancelled() {
                        break;
                    }
                    let meta = match &bulk {
                        Some(entries) => entries.get(std::ffi::OsStr::new(name)).copied(),
                        None => std::fs::symlink_metadata(group.path.join(name))
                            .ok()
                            .map(Into::into),
                    };
                    updates.push(HydratedRow {
                        id,
                        parent: group.parent,
                        name,
                        metadata: meta
                            .map(crate::SlabNodeMetadataCompact::some)
                            .unwrap_or_else(crate::SlabNodeMetadataCompact::unaccessible),
                    });
                    if updates.len() >= 1024 {
                        publish(std::mem::take(&mut updates));
                    }
                }
            }
            if !updates.is_empty() {
                publish(updates);
            }
        });
    }
}

pub struct NumericOrder {
    rows: Vec<NumericRow>,
    pub missing: usize,
    pub to_hydrate: usize,
}

struct NumericRow {
    id: SlabIndex,
    value: i64,
    name_rank: u32,
    type_order: u8,
    path_rank: u32,
    missing: bool,
}

impl NumericOrder {
    pub fn sort(mut self, token: CancellationToken) -> Option<Vec<SlabIndex>> {
        token.is_cancelled()?;
        self.rows.par_sort_unstable_by_key(|r| {
            (r.missing, r.value, r.name_rank, r.type_order, r.path_rank)
        });
        token.is_cancelled()?;
        Some(self.rows.into_iter().map(|r| r.id).collect())
    }
}

fn numeric_value(meta: crate::SlabNodeMetadataCompact, key: SortKey) -> i64 {
    meta.as_ref()
        .map(|m| match key {
            SortKey::Size => m.size(),
            SortKey::Mtime => m.mtime().map(|v| i64::from(v.get())).unwrap_or(i64::MIN),
            SortKey::Ctime => m.ctime().map(|v| i64::from(v.get())).unwrap_or(i64::MIN),
            _ => 0,
        })
        .unwrap_or(i64::MIN)
}

fn type_order(meta: crate::SlabNodeMetadataCompact) -> u8 {
    if meta.is_unaccessible() {
        2
    } else if meta.file_type_hint() == NodeFileType::Dir {
        0
    } else {
        1
    }
}

impl SearchCache {
    /// Emit full paths in byte order by visiting a directory entry and its
    /// "name/" prefix separately. For example /a, /a.txt, /a/child.
    /// This avoids materializing and comparing millions of absolute paths.
    pub fn path_order(
        &self,
        ids: &[SlabIndex],
        token: CancellationToken,
    ) -> Option<Vec<SlabIndex>> {
        if ids.len().saturating_mul(8) < self.get_total_files() {
            let mut paths = Vec::with_capacity(ids.len());
            for (i, &id) in ids.iter().enumerate() {
                token.is_cancelled_sparse(i)?;
                if let Some(path) = self.node_path(id) {
                    paths.push((path, id));
                }
            }
            paths.sort_unstable_by(|(a, _), (b, _)| a.as_os_str().cmp(b.as_os_str()));
            return Some(paths.into_iter().map(|(_, id)| id).collect());
        }
        let max = ids.iter().map(SlabIndex::get).max().unwrap_or(0);
        let mut included = vec![0u64; max / 64 + 1];
        for id in ids {
            let i = id.get();
            included[i / 64] |= 1 << (i % 64);
        }
        let root = self.file_nodes.root();
        let mut stack = vec![(root, true), (root, false)];
        let mut order = Vec::with_capacity(ids.len());
        let mut visits = 0;
        while let Some((id, descend)) = stack.pop() {
            visits += 1;
            token.is_cancelled_sparse(visits)?;
            if !descend {
                let i = id.get();
                if included
                    .get(i / 64)
                    .is_some_and(|word| word & (1 << (i % 64)) != 0)
                {
                    order.push(id);
                }
                continue;
            }
            let from = stack.len();
            for &child in &self.file_nodes[id].children {
                stack.push((child, false));
                if !self.file_nodes[child].children.is_empty() {
                    stack.push((child, true));
                }
            }
            stack[from..].sort_unstable_by(|&(a, ad), &(b, bd)| {
                let a = self.file_nodes[a].name().as_bytes();
                let b = self.file_nodes[b].name().as_bytes();
                let common = a.len().min(b.len());
                b[..common].cmp(&a[..common]).then_with(|| {
                    let next = |name: &[u8], descend| {
                        name.get(common)
                            .copied()
                            .or(if descend { Some(b'/') } else { None })
                    };
                    next(b, bd).cmp(&next(a, ad))
                })
            });
        }
        Some(order)
    }

    /// Assign lexical ranks once; sorting then compares integers and never accesses disk.
    pub fn indexed_numeric_order(
        &self,
        ids: &[SlabIndex],
        key: SortKey,
        name_ordered: bool,
        token: CancellationToken,
    ) -> Option<NumericOrder> {
        let paths = self.path_order(ids, token)?;
        let mut path_ranks = vec![0u32; ids.iter().map(SlabIndex::get).max().unwrap_or(0) + 1];
        for (rank, id) in paths.into_iter().enumerate() {
            path_ranks[id.get()] = rank as u32;
        }
        let name = |id: SlabIndex| self.file_nodes.get(id).map_or("", |node| node.name());
        let mut by_name = Vec::new();
        let ids = if name_ordered && ids.windows(2).all(|pair| name(pair[0]) <= name(pair[1])) {
            ids
        } else {
            by_name.extend_from_slice(ids);
            by_name.par_sort_unstable_by(|&a, &b| name(a).cmp(name(b)));
            &by_name
        };
        let root = self.file_nodes.root();
        let mut rows = Vec::with_capacity(ids.len());
        let mut previous = None;
        let mut rank = 0u32;
        let mut root_position = None;
        let mut root_rank = None;
        let mut missing = 0;
        let mut to_hydrate = 0;
        for (i, &id) in ids.iter().enumerate() {
            token.is_cancelled_sparse(i)?;
            let Some(node) = self.file_nodes.get(id) else {
                continue;
            };
            if id == root && !self.file_nodes.index_only() {
                root_position = Some(rows.len());
            } else if previous != Some(node.name()) {
                if root_rank.is_none() && node.name() >= "/" {
                    rank += 1;
                    root_rank = Some(rank);
                }
                rank += 1;
                previous = Some(node.name());
            }
            let meta = node.metadata;
            let value = numeric_value(meta, key);
            let missing_value = value == i64::MIN;
            missing += usize::from(missing_value);
            to_hydrate += usize::from(meta.is_none());
            rows.push(NumericRow {
                id,
                value,
                name_rank: rank,
                type_order: type_order(meta),
                path_rank: path_ranks[id.get()],
                missing: missing_value,
            });
        }
        if let Some(pos) = root_position {
            rows[pos].name_rank = root_rank.unwrap_or(rank + 1);
        }
        Some(NumericOrder {
            rows,
            missing,
            to_hydrate,
        })
    }

    pub fn remember_hydrated_rows(&mut self, rows: Vec<HydratedRow>) {
        for row in rows {
            if let Some(node) = self.file_nodes.get_mut(row.id)
                && node.parent() == row.parent
                && (node.name() == row.name || row.parent.is_none())
                && node.metadata.is_none()
            {
                node.metadata = row.metadata;
            }
        }
    }

    pub fn publish_hydrated_metadata(&mut self) {
        if let Some(changes) = &mut self.view_changes {
            changes.invalidate_order();
        }
    }

    /// Capture a coherent view on the index thread. Text sorting never stats
    /// files; numeric sorting fills only missing metadata and retains it in the
    /// index, where FSEvents already update it. Missing/deleted IDs are skipped.
    pub fn prepare_sort(
        &mut self,
        indices: &[SlabIndex],
        key: SortKey,
        token: CancellationToken,
    ) -> Option<SortSnapshot> {
        token.is_cancelled()?;
        let numeric_key = matches!(key, SortKey::Size | SortKey::Mtime | SortKey::Ctime);
        if numeric_key && !self.file_nodes.index_only() {
            self.warm_sort_metadata(indices, token)?;
        }
        let mut entries = Vec::with_capacity(indices.len());
        let mut directories = HashMap::new();
        for &index in indices {
            token.is_cancelled()?;
            if let Some(entry) = self.sort_entry(index, numeric_key, key, &mut directories) {
                entries.push(entry);
            }
        }
        token.is_cancelled()?;
        Some(SortSnapshot { entries, key })
    }

    // r1/r2 snapshots can have names but no metadata. Hydrate densely
    // represented directories in one enumeration. A narrow query still
    // uses one lstat per missing result, never a walk of unrelated siblings.
    fn warm_sort_metadata(
        &mut self,
        indices: &[SlabIndex],
        token: CancellationToken,
    ) -> Option<()> {
        let mut counts = HashMap::<SlabIndex, usize>::new();
        for (i, &id) in indices.iter().enumerate() {
            token.is_cancelled_sparse(i)?;
            if let Some(node) = self.file_nodes.get(id)
                && node.metadata.is_none()
                && let Some(parent) = node.parent()
            {
                *counts.entry(parent).or_default() += 1;
            }
        }
        for (parent, count) in counts {
            token.is_cancelled()?;
            if count < 16 || count.saturating_mul(4) < self.file_nodes[parent].children.len() {
                continue;
            }
            let path = self.node_path(parent)?;
            let Ok(entries) =
                fswalk::read_directory_metadata(&path, || token.is_cancelled().is_none())
            else {
                continue;
            };
            let entries = entries?;
            let missing: HashMap<_, _> = self.file_nodes[parent]
                .children
                .iter()
                .filter_map(|&id| {
                    let node = &self.file_nodes[id];
                    node.metadata.is_none().then_some((node.name(), id))
                })
                .collect();
            for entry in entries {
                if let Some(metadata) = entry.metadata
                    && let Some(id) = entry.name.to_str().and_then(|name| missing.get(name))
                {
                    self.file_nodes[*id].metadata = crate::SlabNodeMetadataCompact::some(metadata);
                }
            }
        }
        Some(())
    }

    fn sort_entry(
        &mut self,
        index: SlabIndex,
        numeric_key: bool,
        key: SortKey,
        directories: &mut HashMap<SlabIndex, Arc<str>>,
    ) -> Option<SortEntry> {
        let node = self.file_nodes.get(index)?;
        let (name, directory) = if let Some(parent) = node.parent() {
            let directory = directories.entry(parent).or_insert_with(|| {
                let mut path = self
                    .node_path(parent)
                    .expect("indexed parent")
                    .to_string_lossy()
                    .into_owned();
                if !path.ends_with('/') {
                    path.push('/');
                }
                Arc::from(path)
            });
            (node.name(), Arc::clone(directory))
        } else if self.file_nodes.index_only() {
            let parent = self
                .file_nodes
                .path()
                .parent()
                .unwrap_or(std::path::Path::new("/"));
            (node.name(), Arc::from(format!("{}/", parent.display())))
        } else {
            ("/", Arc::from(""))
        };
        let metadata = if numeric_key {
            self.ensure_metadata(index)
        } else {
            node.metadata
        };
        let numeric = numeric_value(metadata, key);
        let type_order = type_order(metadata);
        Some(SortEntry {
            index,
            name,
            directory,
            numeric,
            type_order,
        })
    }

    /// Keep the sorted view after small changes. Only the changed IDs and the
    /// logarithmic insertion probes need sort keys. All other IDs retain order.
    pub fn patch_sorted_order(
        &mut self,
        order: &mut Vec<SlabIndex>,
        key: SortKey,
        dirty: &[SlabIndex],
        members: &[SlabIndex],
        token: CancellationToken,
    ) -> Option<Vec<ResultEdit>> {
        token.is_cancelled()?;
        let dirty_set: HashSet<_> = dirty.iter().copied().collect();
        let mut removed = Vec::new();
        for (i, id) in order.iter().enumerate() {
            token.is_cancelled_sparse(i)?;
            if dirty_set.contains(id) {
                removed.push(i);
            }
        }
        let retained = order.len() - removed.len();
        // removed[i] - i is the rank of that hole in the retained sequence.
        let holes: Vec<_> = removed.iter().enumerate().map(|(i, at)| at - i).collect();
        let original = |rank| rank + holes.partition_point(|&hole| hole <= rank);
        let mut edits = BTreeMap::new();
        for at in removed {
            edits.insert(
                at,
                ResultEdit {
                    at,
                    remove: 1,
                    insert: vec![],
                },
            );
        }
        let numeric = matches!(key, SortKey::Size | SortKey::Mtime | SortKey::Ctime);
        let mut directories = HashMap::new();
        let mut additions: Vec<_> = members
            .iter()
            .filter_map(|&id| self.sort_entry(id, numeric, key, &mut directories))
            .collect();
        additions.sort_unstable_by(|a, b| compare_entries(a, b, key, SortDirection::Asc));
        let mut probes = HashMap::new();
        for entry in additions {
            token.is_cancelled()?;
            let (mut lo, mut hi) = (0, retained);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let id = order[original(mid)];
                if let std::collections::hash_map::Entry::Vacant(slot) = probes.entry(id) {
                    slot.insert(self.sort_entry(id, numeric, key, &mut directories)?);
                }
                if compare_entries(&probes[&id], &entry, key, SortDirection::Asc).is_lt() {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            let at = original(lo);
            edits
                .entry(at)
                .or_insert_with(|| ResultEdit {
                    at,
                    remove: 0,
                    insert: vec![],
                })
                .insert
                .push(entry.index);
        }
        let mut merged: Vec<ResultEdit> = Vec::with_capacity(edits.len());
        for edit in edits.into_values() {
            if let Some(last) = merged.last_mut()
                && last.at + last.remove == edit.at
            {
                last.remove += edit.remove;
                last.insert.extend(edit.insert);
            } else {
                merged.push(edit);
            }
        }
        merged.retain(|edit| order[edit.at..edit.at + edit.remove] != edit.insert);
        token.is_cancelled()?;
        apply_edits(order, &merged);
        Some(merged)
    }
}

fn compare_paths(a: &SortEntry, b: &SortEntry) -> Ordering {
    if Arc::ptr_eq(&a.directory, &b.directory) || a.directory == b.directory {
        return a.name.cmp(b.name);
    }
    // A parent prefix ends in '/'. Comparing parent and basename separately
    // would order "/a.z" and "/a/z" incorrectly.
    if let Some(tail) = a.directory.strip_prefix(b.directory.as_ref()) {
        return tail.bytes().chain(a.name.bytes()).cmp(b.name.bytes());
    }
    if let Some(tail) = b.directory.strip_prefix(a.directory.as_ref()) {
        return a.name.bytes().cmp(tail.bytes().chain(b.name.bytes()));
    }
    a.directory.cmp(&b.directory)
}

fn compare_entries(
    a: &SortEntry,
    b: &SortEntry,
    key: SortKey,
    direction: SortDirection,
) -> Ordering {
    let ordering = match key {
        SortKey::FullPath => compare_paths(a, b),
        SortKey::Filename => a
            .name
            .cmp(b.name)
            .then_with(|| a.type_order.cmp(&b.type_order))
            .then_with(|| compare_paths(a, b)),
        _ => a
            .numeric
            .cmp(&b.numeric)
            .then_with(|| a.name.cmp(b.name))
            .then_with(|| a.type_order.cmp(&b.type_order))
            .then_with(|| compare_paths(a, b)),
    }
    .then_with(|| a.index.get().cmp(&b.index.get()));
    match direction {
        SortDirection::Asc => ordering,
        SortDirection::Desc => ordering.reverse(),
    }
}

struct MergeHead<'a> {
    entry: &'a SortEntry,
    position: usize,
    end: usize,
    key: SortKey,
    direction: SortDirection,
}
impl PartialEq for MergeHead<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for MergeHead<'_> {}
impl PartialOrd for MergeHead<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for MergeHead<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max heap; reverse to merge from the smallest key.
        compare_entries(self.entry, other.entry, self.key, self.direction).reverse()
    }
}

impl SortSnapshot {
    pub fn missing_numeric(&self) -> usize {
        self.entries
            .iter()
            .filter(|row| row.numeric == i64::MIN)
            .count()
    }

    /// Sort owned keys away from the index thread. Bound cancellation latency
    /// with sorted runs and a cancellable merge, never a stateful comparator.
    pub fn sort(
        mut self,
        direction: SortDirection,
        token: CancellationToken,
    ) -> Option<Vec<SlabIndex>> {
        const RUN_SIZE: usize = 4096;
        token.is_cancelled()?;
        let key = self.key;
        let mut already_ordered = true;
        for (i, pair) in self.entries.windows(2).enumerate() {
            if i.is_multiple_of(1024) {
                token.is_cancelled()?;
            }
            if compare_entries(&pair[0], &pair[1], key, direction) == Ordering::Greater {
                already_ordered = false;
                break;
            }
        }
        if already_ordered {
            let result = self.entries.into_iter().map(|entry| entry.index).collect();
            token.is_cancelled()?;
            return Some(result);
        }
        if self.entries.len() >= 65_536 {
            self.entries
                .par_sort_unstable_by(|a, b| compare_entries(a, b, key, direction));
            token.is_cancelled()?;
            return Some(self.entries.into_iter().map(|e| e.index).collect());
        }
        for run in self.entries.chunks_mut(RUN_SIZE) {
            token.is_cancelled()?;
            run.sort_unstable_by(|a, b| compare_entries(a, b, key, direction));
        }
        let mut heap = BinaryHeap::new();
        for position in (0..self.entries.len()).step_by(RUN_SIZE) {
            heap.push(MergeHead {
                entry: &self.entries[position],
                position,
                end: (position + RUN_SIZE).min(self.entries.len()),
                key,
                direction,
            });
        }
        let mut result = Vec::with_capacity(self.entries.len());
        while let Some(mut head) = heap.pop() {
            if result.len().is_multiple_of(1024) {
                token.is_cancelled()?;
            }
            result.push(head.entry.index);
            head.position += 1;
            if head.position < head.end {
                head.entry = &self.entries[head.position];
                heap.push(head);
            }
        }
        token.is_cancelled()?;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NAME_POOL, SearchOptions, SlabNode, SlabNodeMetadataCompact};
    use cardinal_sdk::{EventFlag, FsEvent};
    use fswalk::NodeMetadata;
    use std::{fs, num::NonZeroU64, path::PathBuf, sync::atomic::AtomicBool};

    static STOP: AtomicBool = AtomicBool::new(false);

    fn meta(kind: NodeFileType, size: u64, mtime: u64, ctime: u64) -> SlabNodeMetadataCompact {
        SlabNodeMetadataCompact::some(NodeMetadata {
            r#type: kind,
            size,
            mtime: NonZeroU64::new(mtime),
            ctime: NonZeroU64::new(ctime),
        })
    }

    fn empty_cache() -> (SearchCache, SlabIndex) {
        let mut cache = SearchCache::noop(PathBuf::from("/"), vec![], vec![], &STOP);
        let root =
            cache
                .file_nodes
                .insert(SlabNode::new(None, "", meta(NodeFileType::Dir, 0, 0, 0)));
        (cache, root)
    }

    fn add(
        cache: &mut SearchCache,
        parent: SlabIndex,
        name: &str,
        metadata: SlabNodeMetadataCompact,
    ) -> SlabIndex {
        cache
            .file_nodes
            .insert(SlabNode::new(Some(parent), NAME_POOL.push(name), metadata))
    }

    fn sorted(
        cache: &mut SearchCache,
        ids: &[SlabIndex],
        key: SortKey,
        direction: SortDirection,
    ) -> Vec<SlabIndex> {
        cache
            .prepare_sort(ids, key, CancellationToken::noop())
            .unwrap()
            .sort(direction, CancellationToken::noop())
            .unwrap()
    }

    #[test]
    fn ranked_paths_and_numbers_match_reference_and_leave_unread_rows_for_hydration() {
        let (mut cache, root) = empty_cache();
        let mut insert = |parent, name, metadata| {
            let id = add(&mut cache, parent, name, metadata);
            cache.file_nodes[parent].add_children(id);
            id
        };
        let dir = meta(NodeFileType::Dir, 0, 2, 2);
        let file = meta(NodeFileType::File, 4, 3, 3);
        let a = insert(root, "a", dir);
        let b = insert(root, "a.z", dir);
        let x = insert(a, "same", file);
        let y = insert(b, "same", file);
        let punct = insert(root, "!first", file);
        let unknown = insert(a, "no-stat", SlabNodeMetadataCompact::none());
        let denied = insert(b, "no-access", SlabNodeMetadataCompact::unaccessible());
        let ids = [y, punct, a, root, x, b];
        let token = CancellationToken::noop();
        assert_eq!(
            cache.path_order(&ids, token).unwrap(),
            sorted(&mut cache, &ids, SortKey::FullPath, SortDirection::Asc)
        );
        for key in [SortKey::Size, SortKey::Mtime, SortKey::Ctime] {
            let expected = sorted(&mut cache, &ids, key, SortDirection::Asc);
            let with_unknown = [y, unknown, b, punct, a, root, denied, x];
            let ranked = cache
                .indexed_numeric_order(&with_unknown, key, false, token)
                .unwrap();
            let known = expected
                .iter()
                .filter(|&&id| numeric_value(cache.file_nodes[id].metadata, key) != i64::MIN)
                .copied()
                .collect::<Vec<_>>();
            assert_eq!(ranked.missing, with_unknown.len() - known.len());
            assert_eq!(ranked.to_hydrate, 1);
            let actual = ranked.sort(token).unwrap();
            assert_eq!(&actual[..known.len()], known);
            assert!(actual[known.len()..].contains(&unknown));
            assert!(actual[known.len()..].contains(&denied));
        }
        assert!(cache.file_nodes[unknown].metadata.is_none());
    }

    #[test]
    fn text_sort_over_20000_preserves_all_ids_without_metadata_io() {
        let (mut cache, root) = empty_cache();
        let ids: Vec<_> = (0..25_001)
            .rev()
            .map(|i| {
                add(
                    &mut cache,
                    root,
                    &format!("item-{i:06}"),
                    SlabNodeMetadataCompact::none(),
                )
            })
            .collect();
        for key in [SortKey::Filename, SortKey::FullPath] {
            let ordered = sorted(&mut cache, &ids, key, SortDirection::Asc);
            assert_eq!(ordered.len(), ids.len());
            assert_eq!(cache.file_nodes[ordered[0]].name(), "item-000000");
            assert_eq!(
                cache.file_nodes[*ordered.last().unwrap()].name(),
                "item-025000"
            );
            assert!(
                ids.iter()
                    .all(|&id| cache.file_nodes[id].metadata.is_none()),
                "text sorting must not stat nonexistent paths"
            );
            let descending = sorted(&mut cache, &ids, key, SortDirection::Desc);
            assert!(ordered.iter().rev().eq(descending.iter()));
        }
    }

    #[test]
    fn merge_across_run_boundaries_matches_a_reference_sort() {
        let entries: Vec<_> = (0..12_003)
            .map(|i| SortEntry {
                index: SlabIndex::new(i),
                name: NAME_POOL.push(&format!("item-{}", (i * 7919) % 37)),
                directory: Arc::from(format!("/group-{}/", i % 7)),
                numeric: ((i * 3571) % 101) as i64,
                type_order: (i % 3) as u8,
            })
            .collect();
        for key in [
            SortKey::Filename,
            SortKey::FullPath,
            SortKey::Size,
            SortKey::Mtime,
            SortKey::Ctime,
        ] {
            for direction in [SortDirection::Asc, SortDirection::Desc] {
                let mut expected = entries.clone();
                expected.sort_by(|a, b| compare_entries(a, b, key, direction));
                let actual = SortSnapshot {
                    entries: entries.clone(),
                    key,
                }
                .sort(direction, CancellationToken::noop())
                .unwrap();
                assert!(expected.iter().map(|entry| entry.index).eq(actual));
            }
        }
    }

    #[test]
    fn cold_filesystem_walk_keeps_empty_directories_before_same_named_files() {
        let temp = tempdir::TempDir::new("sort_directory_hint").unwrap();
        fs::create_dir_all(temp.path().join("a")).unwrap();
        fs::write(temp.path().join("a/same"), b"").unwrap();
        fs::create_dir_all(temp.path().join("b/same")).unwrap();
        let mut cache = SearchCache::walk_fs(temp.path());
        let ids = cache
            .search_with_options(
                "/same/",
                SearchOptions::default(),
                CancellationToken::noop(),
            )
            .unwrap()
            .nodes
            .unwrap();
        assert_eq!(ids.len(), 2);
        let result = sorted(&mut cache, &ids, SortKey::Filename, SortDirection::Asc);
        assert!(cache.node_path(result[0]).unwrap().ends_with("b/same"));
        assert!(cache.file_nodes[result[1]].metadata.is_none());
    }

    #[test]
    fn equal_names_use_directory_type_then_path_and_ignore_deleted_ids() {
        let (mut cache, root) = empty_cache();
        let a = add(&mut cache, root, "a", meta(NodeFileType::Dir, 0, 0, 0));
        let b = add(&mut cache, root, "b", meta(NodeFileType::Dir, 0, 0, 0));
        let file = add(&mut cache, a, "same", SlabNodeMetadataCompact::none());
        let dir = add(&mut cache, b, "same", meta(NodeFileType::Dir, 0, 0, 0));
        let ids = [file, SlabIndex::new(9999), dir];
        assert_eq!(
            sorted(&mut cache, &ids, SortKey::Filename, SortDirection::Asc),
            vec![dir, file]
        );
        assert_eq!(
            sorted(&mut cache, &ids, SortKey::FullPath, SortDirection::Asc),
            vec![file, dir]
        );
    }

    #[test]
    fn numeric_keys_sort_both_directions_with_missing_metadata() {
        let (mut cache, root) = empty_cache();
        let low = add(&mut cache, root, "a", meta(NodeFileType::File, 2, 10, 30));
        let high = add(&mut cache, root, "b", meta(NodeFileType::File, 8, 20, 40));
        let missing = add(
            &mut cache,
            root,
            "c",
            SlabNodeMetadataCompact::unaccessible(),
        );
        for key in [SortKey::Size, SortKey::Mtime, SortKey::Ctime] {
            assert_eq!(
                sorted(&mut cache, &[high, missing, low], key, SortDirection::Asc),
                vec![missing, low, high]
            );
            assert_eq!(
                sorted(&mut cache, &[high, missing, low], key, SortDirection::Desc),
                vec![high, low, missing]
            );
        }
    }

    #[test]
    fn size_sort_observes_events_for_the_same_ids_and_retries_failed_metadata_only_after_change() {
        let temp = tempdir::TempDir::new("sort_events").unwrap();
        let a = temp.path().join("item-a");
        fs::write(&a, b"aa").unwrap();
        fs::write(temp.path().join("item-b"), b"bbbb").unwrap();
        let mut cache = SearchCache::walk_fs(temp.path());
        let ids = cache
            .search_with_options("item-", SearchOptions::default(), CancellationToken::noop())
            .unwrap()
            .nodes
            .unwrap();
        let first = sorted(&mut cache, &ids, SortKey::Size, SortDirection::Asc);
        assert!(cache.node_path(first[0]).unwrap().ends_with("item-a"));
        cache.file_nodes[first[0]].metadata = SlabNodeMetadataCompact::unaccessible();
        sorted(&mut cache, &ids, SortKey::Size, SortDirection::Asc);
        assert!(cache.file_nodes[first[0]].metadata.is_unaccessible());
        fs::write(&a, b"aaaaaaaa").unwrap();
        let event_id = cache.last_event_id() + 1;
        cache
            .handle_fs_events(vec![FsEvent {
                path: a,
                id: event_id,
                flag: EventFlag::ItemModified,
            }])
            .unwrap();
        let next = sorted(&mut cache, &ids, SortKey::Size, SortDirection::Asc);
        assert!(cache.node_path(next[0]).unwrap().ends_with("item-b"));
        assert_eq!(
            cache.file_nodes[*next.last().unwrap()]
                .metadata
                .as_ref()
                .unwrap()
                .size(),
            8
        );
    }

    #[test]
    fn cancelled_snapshot_is_not_published() {
        let (mut cache, root) = empty_cache();
        let id = add(&mut cache, root, "a", SlabNodeMetadataCompact::none());
        let token = CancellationToken::new_sort();
        let snapshot = cache
            .prepare_sort(&[id], SortKey::Filename, CancellationToken::noop())
            .unwrap();
        CancellationToken::cancel_sort();
        assert!(cache.prepare_sort(&[id], SortKey::Size, token).is_none());
        assert!(snapshot.sort(SortDirection::Asc, token).is_none());
        assert!(cache.file_nodes[id].metadata.is_none());
    }
}
