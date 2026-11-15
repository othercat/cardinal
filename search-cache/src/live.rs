//! The desktop window holds one query. Between two responses, collect the first
//! name of each changed slab slot so removals and ID reuse can be located in
//! the previous ordered result without scanning the index again.
use crate::{
    SearchCache, SearchOptions, SearchQuery, SegmentMatcher, SlabIndex, build_segment_matchers,
    query_preprocessor::{expand_query_home_dirs, strip_query_quotes},
};
use anyhow::Result;
use cardinal_syntax::{Expr, Term, optimize_query, parse_query};
use query_segmentation::query_segmentation;
use search_cancel::CancellationToken;
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

const MAX_CHANGES: usize = 2048;
pub const INLINE_RESULTS: usize = 8192;
pub const VIEW_PAGE_SIZE: usize = 256;

#[derive(Default)]
pub(crate) struct IndexChanges {
    first_names: HashMap<SlabIndex, &'static str>,
    paths: HashSet<PathBuf>,
    overflow: bool,
    pub(crate) removed_or_modified: bool,
}

impl IndexChanges {
    pub(crate) fn invalidate_order(&mut self) {
        self.overflow = true;
    }
    pub(crate) fn record(&mut self, id: SlabIndex, name: &'static str) {
        if !self.overflow {
            self.first_names.entry(id).or_insert(name);
            self.overflow = self.first_names.len() > MAX_CHANGES;
        }
    }

    pub(crate) fn record_path(&mut self, path: &Path) {
        if !self.overflow {
            self.paths.insert(path.to_owned());
            self.overflow = self.paths.len() > MAX_CHANGES;
        }
    }
}

/// Coordinates refer to the original list. Edits are disjoint, ascending.
#[derive(Debug, Clone, Serialize)]
pub struct ResultEdit {
    pub at: usize,
    pub remove: usize,
    pub insert: Vec<SlabIndex>,
}

pub(crate) fn apply_edits(ids: &mut Vec<SlabIndex>, edits: &[ResultEdit]) {
    match edits {
        [] => {}
        [edit] => {
            ids.splice(edit.at..edit.at + edit.remove, edit.insert.iter().copied());
        }
        _ => {
            let mut next = Vec::with_capacity(ids.len());
            let mut cursor = 0;
            for edit in edits {
                next.extend_from_slice(&ids[cursor..edit.at]);
                next.extend_from_slice(&edit.insert);
                cursor = edit.at + edit.remove;
            }
            next.extend_from_slice(&ids[cursor..]);
            *ids = next;
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewUpdate {
    pub view_id: u64,
    pub incremental: bool,
    pub results: Vec<SlabIndex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<usize>,
    pub highlights: Vec<String>,
    pub status_code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<ResultEdit>>,
    // The event notification and command travel independently. These paths
    // cover everything actually consumed by this revision, including later events.
    pub changed_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_ids: Option<Vec<SlabIndex>>,
}

#[derive(Default)]
pub struct LiveSearch {
    version: u64,
    query: SearchQuery,
    options: SearchOptions,
    matcher: Option<SegmentMatcher>,
    nodes: Vec<SlabIndex>,
    highlights: Vec<String>,
    last_delta: Option<(u64, Vec<SlabIndex>, Vec<SlabIndex>)>,
}

impl LiveSearch {
    pub fn nodes(&self, version: u64) -> Option<&[SlabIndex]> {
        (version != 0 && version == self.version).then_some(&self.nodes)
    }

    pub fn name_ordered(&self) -> bool {
        self.matcher.is_some()
    }

    /// Name-only searches already have a global primary-key order. Refine just the
    /// equal-name buckets touching the requested window, including both edges.
    pub fn filename_page(
        &self,
        cache: &mut SearchCache,
        start: usize,
        len: usize,
        descending: bool,
    ) -> Vec<SlabIndex> {
        let n = self.nodes.len();
        let start = start.min(n);
        let len = len.min(n - start);
        if len == 0 {
            return Vec::new();
        }
        let first = if descending { n - start - len } else { start };
        let last = first + len - 1;
        let old_name = |id: SlabIndex| {
            cache
                .view_changes
                .as_ref()
                .and_then(|changes| changes.first_names.get(&id).copied())
                .or_else(|| cache.file_nodes.get(id).map(|node| node.name()))
                .unwrap_or("")
        };
        // The stored name of the volume root can be empty. The Filename column
        // displays '/', which sorts after names beginning with punctuation.
        let root = cache.file_nodes.root();
        let root_rank = if self.nodes.first() == Some(&root) && old_name(root).is_empty() {
            Some(self.nodes.partition_point(|&id| old_name(id) < "/") - 1)
        } else {
            None
        };
        let id_at = |i: usize| match root_rank {
            Some(r) if i == r => root,
            Some(r) if i < r => self.nodes[i + 1],
            _ => self.nodes[i],
        };
        let name_at = |i| {
            let id = id_at(i);
            if id == root { "/" } else { old_name(id) }
        };
        let mut a = first;
        let mut b = last;
        while a > 0 && name_at(a - 1) == name_at(first) {
            a -= 1;
        }
        while b + 1 < n && name_at(b + 1) == name_at(last) {
            b += 1;
        }
        let source: Vec<_> = (a..=b).map(id_at).collect();
        let token = CancellationToken::noop();
        let ordered = cache
            .prepare_sort(&source, crate::SortKey::Filename, token)
            .and_then(|snapshot| snapshot.sort(crate::SortDirection::Asc, token))
            .unwrap_or_default();
        let mut page: Vec<_> = ordered.into_iter().skip(first - a).take(len).collect();
        if descending {
            page.reverse();
        }
        page
    }
    pub fn filename_positions(
        &self,
        cache: &mut SearchCache,
        wanted: &[SlabIndex],
        descending: bool,
    ) -> Vec<u32> {
        let n = self.nodes.len();
        wanted
            .iter()
            .map(|&id| {
                let node = cache.file_nodes.get(id)?;
                let name = if node.parent().is_none() {
                    "/"
                } else {
                    node.name()
                };
                let root = cache.file_nodes.root();
                let moved_root =
                    self.nodes.first() == Some(&root) && cache.file_nodes[root].name().is_empty();
                let get_name = |i: &SlabIndex| {
                    cache
                        .view_changes
                        .as_ref()
                        .and_then(|changes| changes.first_names.get(i).copied())
                        .or_else(|| cache.file_nodes.get(*i).map(|node| node.name()))
                        .unwrap_or("")
                };
                let start = self.nodes.partition_point(|i| get_name(i) < name);
                let count = self.nodes[start..].partition_point(|i| get_name(i) == name);
                let first = if moved_root && name <= "/" {
                    start.saturating_sub(1)
                } else {
                    start
                };
                let count = count + usize::from(moved_root && name == "/");
                let page = self.filename_page(cache, first, count, false);
                let pos = first + page.iter().position(|&i| i == id)?;
                Some((if descending { n - 1 - pos } else { pos }) as u32)
            })
            .map(|pos| pos.unwrap_or(u32::MAX))
            .collect()
    }

    /// Removed/modified IDs to discard and current matching IDs to reinsert.
    pub fn delta_since(&self, base: u64) -> Option<(&[SlabIndex], &[SlabIndex])> {
        let (from, dirty, members) = self.last_delta.as_ref()?;
        (*from == base).then_some((dirty, members))
    }

    pub fn search(
        &mut self,
        cache: &mut SearchCache,
        query: SearchQuery,
        options: SearchOptions,
        base: Option<u64>,
        token: CancellationToken,
    ) -> Result<ViewUpdate> {
        self.search_impl(cache, query, options, base, token, false)
    }

    pub fn search_windowed(
        &mut self,
        cache: &mut SearchCache,
        query: SearchQuery,
        options: SearchOptions,
        base: Option<u64>,
        token: CancellationToken,
    ) -> Result<ViewUpdate> {
        self.search_impl(cache, query, options, base, token, true)
    }

    fn search_impl(
        &mut self,
        cache: &mut SearchCache,
        query: SearchQuery,
        options: SearchOptions,
        base: Option<u64>,
        token: CancellationToken,
        windowed: bool,
    ) -> Result<ViewUpdate> {
        // Never consume a journal for a cancelled request. If cancellation
        // races with completion, the next caller's cursor forces a full reply.
        if token.is_cancelled().is_none() {
            return Ok(self.response(1, vec![], None, vec![]));
        }
        let append_only = base == Some(self.version)
            && self.query == query
            && cache
                .view_changes
                .as_ref()
                .is_some_and(|changes| !changes.removed_or_modified);
        if base == Some(self.version)
            && self.query == query
            && self.options.case_insensitive == options.case_insensitive
            && self.matcher.is_some()
            && let Some(changes) = cache.view_changes.take()
            && !changes.overflow
        {
            cache.view_changes = Some(IndexChanges::default());
            let was_paged = windowed && self.nodes.len() > INLINE_RESULTS;
            let mut update = self.patch(cache, changes);
            if windowed && self.nodes.len() > INLINE_RESULTS {
                update.total_count = Some(self.nodes.len());
                update.results = self.nodes[..VIEW_PAGE_SIZE].to_vec();
                if let Some(edits) = &mut update.edits {
                    edits.clear();
                }
            } else if was_paged {
                update.results.clone_from(&self.nodes);
                update.edits = None;
            }
            return Ok(update);
        }
        let outcome = cache.search_query_with_options(query.clone(), options, token)?;
        let Some(nodes) = outcome.nodes else {
            return Ok(self.response(1, vec![], None, vec![]));
        };
        self.matcher = filename_matcher(cache, &query, options);
        self.query = query;
        self.options = options;
        self.version += 1;
        self.nodes = nodes;
        self.highlights = outcome.highlights;
        self.last_delta = None;
        cache.view_changes = self.matcher.as_ref().map(|_| IndexChanges::default());
        let paged = windowed && self.nodes.len() > INLINE_RESULTS;
        let ids = if paged {
            &self.nodes[..VIEW_PAGE_SIZE]
        } else {
            self.nodes.as_slice()
        };
        let mut update = self.response(0, ids.to_vec(), None, vec![]);
        update.total_count = paged.then_some(self.nodes.len());
        if append_only {
            update.changed_ids = Some(vec![]);
        }
        Ok(update)
    }

    fn response(
        &self,
        status_code: u8,
        results: Vec<SlabIndex>,
        edits: Option<Vec<ResultEdit>>,
        changed_paths: Vec<PathBuf>,
    ) -> ViewUpdate {
        ViewUpdate {
            view_id: self.version,
            incremental: self.matcher.is_some(),
            results,
            total_count: None,
            highlights: self.highlights.clone(),
            status_code,
            changed_ids: None,
            edits,
            changed_paths: changed_paths
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        }
    }

    fn patch(&mut self, cache: &SearchCache, changes: IndexChanges) -> ViewUpdate {
        let matches = |name: &str| match self.matcher.as_ref().unwrap() {
            SegmentMatcher::Concrete(matcher) => matcher.matches(name),
            SegmentMatcher::Star | SegmentMatcher::GlobStar => true,
        };
        let mut names = BTreeSet::new();
        let mut dirty = Vec::new();
        let mut members = Vec::new();
        for (&id, &first) in &changes.first_names {
            let current = cache.file_nodes.get(id).map(|node| node.name());
            let previous_match = matches(first);
            if previous_match {
                names.insert(first);
            }
            if let Some(current) = current.filter(|name| matches(name)) {
                names.insert(current);
                members.push(id);
                dirty.push(id);
            } else if previous_match {
                dirty.push(id);
            }
        }
        if dirty.is_empty() {
            return self.response(0, vec![], Some(vec![]), vec![]);
        }
        let old_name = |id: &SlabIndex| {
            changes
                .first_names
                .get(id)
                .copied()
                .unwrap_or_else(|| cache.file_nodes[*id].name())
        };
        let mut edits = Vec::new();
        for name in names {
            let at = self.nodes.partition_point(|id| old_name(id) < name);
            let remove = self.nodes[at..].partition_point(|id| old_name(id) == name);
            let new = cache
                .name_index
                .get(name)
                .map(|bucket| bucket.as_slice())
                .unwrap_or_default();
            // Never retransmit a large same-name bucket for a single create.
            let old = &self.nodes[at..at + remove];
            if old == new {
                continue;
            }
            let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(new[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            edits.push(ResultEdit {
                at: at + prefix,
                remove: remove - prefix - suffix,
                insert: new[prefix..new.len() - suffix].to_vec(),
            });
        }
        apply_edits(&mut self.nodes, &edits);
        let changed_ids = dirty.clone();
        self.last_delta = Some((self.version, dirty, members));
        self.version += 1;
        let mut response =
            self.response(0, vec![], Some(edits), changes.paths.into_iter().collect());
        response.changed_ids = Some(changed_ids);
        response
    }
}

// Only use name buckets when the normal query evaluator returns exactly
// their order. Scoped queries, alternate normalization passes, compositions,
// and metadata/content filters retain the full evaluator.
fn filename_matcher(
    cache: &SearchCache,
    query: &SearchQuery,
    options: SearchOptions,
) -> Option<SegmentMatcher> {
    if query.directory_query.is_some() {
        return None;
    }
    let Some(line) = &query.query else {
        return Some(SegmentMatcher::Star);
    };
    if cache.alternate_normalization_query(line).is_some() {
        return None;
    }
    let parsed = optimize_query(strip_query_quotes(expand_query_home_dirs(
        parse_query(line).ok()?,
    )));
    match parsed.expr {
        Expr::Empty => Some(SegmentMatcher::Star),
        Expr::Term(Term::Word(word)) => {
            let mut matchers = build_segment_matchers(&query_segmentation(&word), options).ok()?;
            if matchers.len() == 1 {
                matchers.pop()
            } else {
                None
            }
        }
        _ => None,
    }
}
