use super::{Volume, Volumes};
use crate::{background::view_page, commands::NodeInfoMetadata, sort::SortStatePayload};
use anyhow::{Context, Result, ensure};
use search_cache::{SearchCache, SearchOptions, SearchQuery, SlabIndex, SortDirection, SortKey};
use search_cancel::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Component, PathBuf},
    sync::atomic::AtomicU64,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VolumeQuery {
    pub query: Option<String>,
    pub directory_query: Option<String>,
    pub case_sensitive: bool,
    pub sort: Option<SortStatePayload>,
    pub offset: usize,
    pub limit: usize,
    pub request_id: u64,
    #[serde(default)]
    pub excluded_paths: Vec<PathBuf>,
}

#[derive(Serialize)]
pub(crate) struct Row {
    path: PathBuf,
    metadata: Option<NodeInfoMetadata>,
}

#[derive(Serialize)]
pub(crate) struct VolumePage {
    total: usize,
    indexed: usize,
    rows: Vec<Row>,
    highlights: Vec<String>,
}

struct Order {
    query: SearchQuery,
    sensitive: bool,
    key: Option<SortKey>,
    ids: Vec<SlabIndex>,
    missing: usize,
    highlights: Vec<String>,
}

pub(super) struct Index {
    pub cache: SearchCache,
    order: Option<Order>,
    excluded_paths: Vec<PathBuf>,
    hidden: HashSet<SlabIndex>,
}

impl Index {
    pub fn new(cache: SearchCache) -> Self {
        Self {
            cache,
            order: None,
            excluded_paths: vec![],
            hidden: HashSet::new(),
        }
    }
    pub fn changed(&mut self) {
        self.order = None;
        self.excluded_paths.clear();
        self.hidden.clear();
    }

    pub fn page(&mut self, request: VolumeQuery, token: CancellationToken) -> Result<VolumePage> {
        if self.excluded_paths != request.excluded_paths {
            let mut hidden = HashSet::new();
            for path in &request.excluded_paths {
                ensure!(
                    !path.as_os_str().is_empty()
                        && path.components().all(|c| matches!(c, Component::Normal(_))),
                    "Expected a volume-relative exclusion"
                );
                if let Some(id) = self
                    .cache
                    .node_index_for_path(&self.cache.root_path().join(path))
                    && hidden.insert(id)
                {
                    hidden.extend(
                        self.cache
                            .all_subnodes(id, token)
                            .context("Search superseded")?,
                    );
                }
            }
            self.hidden = hidden;
            self.excluded_paths = request.excluded_paths;
            self.order = None;
        }
        let query = SearchQuery {
            query: request.query,
            directory_query: request.directory_query,
        };
        let key = request.sort.map(|s| s.key);
        let cached = self.order.as_ref().is_some_and(|o| {
            o.query == query && o.sensitive == request.case_sensitive && o.key == key
        });
        if !cached {
            let outcome = self.cache.search_query_with_options(
                query.clone(),
                SearchOptions {
                    case_insensitive: !request.case_sensitive,
                },
                token,
            )?;
            let mut ids = outcome.nodes.context("Search superseded")?;
            if !self.hidden.is_empty() {
                ids.retain(|id| !self.hidden.contains(id));
                token.is_cancelled().context("Search superseded")?;
            }
            let mut missing = 0;
            if let Some(key) = key {
                ids = match key {
                    SortKey::Size | SortKey::Mtime | SortKey::Ctime if ids.len() > 4096 => {
                        let order = self
                            .cache
                            .indexed_numeric_order(&ids, key, false, token)
                            .context("Search superseded")?;
                        missing = order.missing;
                        order.sort(token)
                    }
                    SortKey::FullPath
                        if ids.len().saturating_mul(8) >= self.cache.get_total_files() =>
                    {
                        self.cache.path_order(&ids, token)
                    }
                    _ => {
                        // Unavailable numeric values stay last in both directions.
                        let snapshot = self
                            .cache
                            .prepare_sort(&ids, key, token)
                            .context("Search superseded")?;
                        if matches!(key, SortKey::Size | SortKey::Mtime | SortKey::Ctime) {
                            missing = snapshot.missing_numeric();
                        }
                        snapshot.sort(SortDirection::Asc, token).map(|mut ids| {
                            ids.rotate_left(missing);
                            ids
                        })
                    }
                }
                .context("Search superseded")?;
            }
            self.order = Some(Order {
                query,
                sensitive: request.case_sensitive,
                key,
                ids,
                missing,
                highlights: outcome.highlights,
            });
        }
        let order = self.order.as_ref().unwrap();
        let descending = request
            .sort
            .is_some_and(|s| s.direction == SortDirection::Desc);
        let ids = view_page(
            &order.ids,
            order.missing,
            descending,
            request.offset,
            request.limit.min(1024),
            None,
        )
        .into_iter()
        .map(|id| SlabIndex::new(id as usize))
        .collect::<Vec<_>>();
        let rows = self
            .cache
            .cached_file_nodes(&ids)
            .into_iter()
            .map(|r| Row {
                path: r.path,
                metadata: r.metadata.as_ref().map(NodeInfoMetadata::from_metadata),
            })
            .collect();
        Ok(VolumePage {
            total: order.ids.len(),
            indexed: self.cache.get_total_files() - self.hidden.len(),
            rows,
            highlights: order.highlights.clone(),
        })
    }
}

static REQUEST: AtomicU64 = AtomicU64::new(0);

#[tauri::command(async)]
pub(crate) fn query_volume(
    id: String,
    request: VolumeQuery,
    volumes: tauri::State<'_, Volumes>,
) -> Result<VolumePage, String> {
    let token = CancellationToken::for_request(&REQUEST, request.request_id);
    let source = volumes.get(&id).map_err(|e| e.to_string())?;
    do_query(&source, request, token).map_err(|e| e.to_string())
}

pub(super) fn do_query(
    source: &Volume,
    request: VolumeQuery,
    token: CancellationToken,
) -> Result<VolumePage> {
    *source.last_query.lock() = std::time::Instant::now();
    let mut data = source.index.lock();
    token.is_cancelled().context("Search superseded")?;
    source.load_snapshot(&mut data)?;
    match data.as_mut() {
        Some(index) => index.page(request, token),
        None => Ok(VolumePage {
            total: 0,
            indexed: 0,
            rows: vec![],
            highlights: vec![],
        }),
    }
}
