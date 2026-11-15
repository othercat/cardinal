use cardinal_sdk::{EventFlag, FsEvent};
use search_cache::{
    InitialScan, LiveSearch, ResultEdit, SearchCache, SearchOptions, SearchQuery, SlabIndex,
    SortDirection, SortKey, WalkData,
};
use search_cancel::CancellationToken;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};
use tempdir::TempDir;

static STOP: AtomicBool = AtomicBool::new(false);
const KEYS: [SortKey; 5] = [
    SortKey::Filename,
    SortKey::FullPath,
    SortKey::Size,
    SortKey::Mtime,
    SortKey::Ctime,
];

fn full_order(cache: &mut SearchCache, ids: &[SlabIndex], key: SortKey) -> Vec<SlabIndex> {
    let token = CancellationToken::noop();
    cache
        .prepare_sort(ids, key, token)
        .unwrap()
        .sort(SortDirection::Asc, token)
        .unwrap()
}
fn apply_client(ids: &mut Vec<SlabIndex>, edits: &[ResultEdit]) {
    for edit in edits.iter().rev() {
        ids.splice(edit.at..edit.at + edit.remove, edit.insert.iter().copied());
    }
}
fn query(word: Option<&str>) -> SearchQuery {
    SearchQuery {
        directory_query: None,
        query: word.map(String::from),
    }
}

#[test]
fn directory_attributes_do_not_walk_descendants_or_hide_child_events() {
    let dir = TempDir::new("cardinal-directory-events").unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut cache = SearchCache::walk_fs(&root);
    let old_cursor = cache.last_event_id();
    let unseen = root.join("without-event");
    let created = root.join("new-folder");
    fs::write(&unseen, "unreported").unwrap();
    fs::create_dir(&created).unwrap();
    fs::write(created.join("child"), "included").unwrap();
    let plan = cache
        .plan_events(&[
            FsEvent {
                path: root,
                flag: EventFlag::ItemIsDir | EventFlag::ItemInodeMetaMod,
                id: old_cursor + 1,
            },
            FsEvent {
                path: created.clone(),
                flag: EventFlag::ItemIsDir | EventFlag::ItemCreated,
                id: old_cursor + 2,
            },
        ])
        .unwrap();
    let replay = plan.start(&STOP);
    loop {
        assert_eq!(cache.last_event_id(), old_cursor);
        let observation = replay
            .observations()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        if cache.apply_observation(observation) {
            break;
        }
    }
    assert_eq!(cache.last_event_id(), old_cursor + 2);
    assert!(cache.node_index_for_path(&unseen).is_none());
    assert!(cache.node_index_for_path(&created.join("child")).is_some());
}
fn update(cache: &mut SearchCache, changes: &[(&Path, EventFlag)]) {
    let seq = cache.last_event_id() + 1;
    cache
        .handle_fs_events(
            changes
                .iter()
                .enumerate()
                .map(|(i, (p, flag))| FsEvent {
                    path: p.to_path_buf(),
                    flag: *flag,
                    id: seq + i as u64,
                })
                .collect(),
        )
        .unwrap();
}

#[test]
fn ordinary_events_patch_raw_and_all_five_global_orders_including_reused_slots() {
    let dir = TempDir::new("cardinal-deltas").unwrap();
    let root = dir.path().canonicalize().unwrap();
    let paths: Vec<_> = [
        "a/item",
        "a.z/item",
        "a/b/item",
        "b/item",
        "b/item-z",
        "b/item-a",
        "b/unrelated",
    ]
    .map(|p| root.join(p))
    .into();
    for path in &paths[..5] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, [1u8; 5]).unwrap();
    }
    let mut cache = SearchCache::walk_fs(&root);
    let token = CancellationToken::noop();
    let options = SearchOptions {
        case_insensitive: true,
    };
    let q = query(Some("ItEm"));
    let mut view = LiveSearch::default();
    let start = view
        .search(&mut cache, q.clone(), options, None, token)
        .unwrap();
    assert!(start.incremental);
    let mut raw = start.results;
    let mut version = start.view_id;
    let mut orders = KEYS.map(|key| full_order(&mut cache, &raw, key));
    let flags = EventFlag::ItemIsFile | EventFlag::ItemModified;
    for step in 0..5 {
        let events = match step {
            0 => {
                fs::write(&paths[3], [8; 30]).unwrap();
                vec![(&paths[3], flags)]
            }
            1 => {
                fs::rename(&paths[4], &paths[5]).unwrap();
                vec![
                    (&paths[4], flags | EventFlag::ItemRenamed),
                    (&paths[5], flags | EventFlag::ItemRenamed),
                ]
            }
            2 => {
                fs::remove_file(&paths[0]).unwrap();
                fs::write(&paths[4], [2]).unwrap();
                vec![
                    (&paths[0], flags | EventFlag::ItemRemoved),
                    (&paths[4], flags | EventFlag::ItemCreated),
                ]
            }
            3 => {
                fs::write(&paths[6], [1]).unwrap();
                vec![(&paths[6], flags)]
            }
            _ => {
                fs::remove_dir_all(root.join("a.z")).unwrap();
                vec![(&paths[1], flags | EventFlag::ItemRemoved)]
            }
        };
        update(
            &mut cache,
            &events
                .into_iter()
                .map(|(p, f)| (p.as_path(), f))
                .collect::<Vec<_>>(),
        );
        let delta = view
            .search(&mut cache, q.clone(), options, Some(version), token)
            .unwrap();
        assert!(delta.results.is_empty());
        apply_client(&mut raw, &delta.edits.unwrap());
        assert_eq!(
            cache
                .search_query_with_options(q.clone(), options, token)
                .unwrap()
                .nodes
                .unwrap(),
            raw
        );
        if step == 3 {
            assert_eq!(version, delta.view_id);
            continue;
        }
        let (dirty, members) = view.delta_since(version).unwrap();
        for (i, key) in KEYS.into_iter().enumerate() {
            let mut client_copy = orders[i].clone();
            let edits = cache
                .patch_sorted_order(&mut orders[i], key, dirty, members, token)
                .unwrap();
            apply_client(&mut client_copy, &edits);
            assert_eq!(orders[i], client_copy);
            assert_eq!(
                client_copy,
                full_order(&mut cache, &raw, key),
                "{key:?} step {step}"
            );
        }
        version = delta.view_id;
    }
    let stale = view
        .search(&mut cache, q, options, Some(start.view_id), token)
        .unwrap();
    assert!(stale.edits.is_none());
    assert_eq!(stale.results, raw);
    let complex = view
        .search(&mut cache, query(Some("size:>2")), options, None, token)
        .unwrap();
    assert!(!complex.incremental);
    assert!(
        view.search(
            &mut cache,
            query(Some("size:>2")),
            options,
            Some(complex.view_id),
            token
        )
        .unwrap()
        .edits
        .is_none()
    );
}

#[test]
fn progressive_scan_is_searchable_before_completion_and_replays_changes_from_its_start() {
    let dir = TempDir::new("cardinal-partial-scan").unwrap();
    let root = dir.path().canonicalize().unwrap();
    let ignored = root.join("ignored");
    fs::create_dir(&ignored).unwrap();
    fs::write(ignored.join("hidden"), "x").unwrap();
    for child in ["a", "a.z", "a/b", "z"] {
        let path = root.join(child);
        fs::create_dir_all(&path).unwrap();
        for name in ["item", "item-中", "second"] {
            fs::write(path.join(name), name.as_bytes()).unwrap();
        }
    }
    std::os::unix::fs::symlink(root.join("a"), root.join("alias")).unwrap();
    let token = CancellationToken::noop();
    let ignores = vec![ignored];
    let (mut cache, mut scan) =
        InitialScan::start(root.clone(), ignores.clone(), vec![], &STOP, token).unwrap();
    let mut view = LiveSearch::default();
    let q = query(None);
    let start = view
        .search(&mut cache, q.clone(), SearchOptions::default(), None, token)
        .unwrap();
    let mut version = start.view_id;
    let mut visible = start.results;
    let mut partial = false;
    let start_cursor = cache.last_event_id();
    loop {
        let batch = scan.batches().recv_timeout(Duration::from_secs(5)).unwrap();
        let done = scan.apply(&mut cache, batch);
        let update = view
            .search(
                &mut cache,
                q.clone(),
                SearchOptions::default(),
                Some(version),
                token,
            )
            .unwrap();
        apply_client(&mut visible, &update.edits.unwrap());
        version = update.view_id;
        assert_eq!(visible, cache.search_empty(token).unwrap());
        if done {
            break;
        }
        partial |= visible.iter().any(|&id| {
            cache
                .node_path(id)
                .is_some_and(|p| p.starts_with(&root) && p != root)
        });
    }
    assert!(partial);
    assert_eq!(cache.last_event_id(), start_cursor);
    let mut reference = SearchCache::walk_fs_with_walk_data(
        &WalkData::new(&root, &ignores, &[], true, || false),
        &STOP,
    )
    .unwrap();
    let reference_ids = reference.search_empty(token).unwrap();
    for key in KEYS {
        let paths = |cache: &mut SearchCache, ids: &[SlabIndex]| -> Vec<PathBuf> {
            full_order(cache, ids, key)
                .into_iter()
                .map(|id| cache.node_path(id).unwrap())
                .collect()
        };
        assert_eq!(
            paths(&mut cache, &visible),
            paths(&mut reference, &reference_ids)
        );
    }
    let db = dir.path().join("snapshot.db");
    cache.flush_to_file(&db).unwrap();
    let mut restored =
        SearchCache::try_read_persistent_cache(&root, &db, &ignores, &[], &STOP).unwrap();
    assert_eq!(restored.search_empty(token).unwrap(), visible);
    let created = root.join("new-at-boundary");
    fs::write(&created, "new").unwrap();
    update(
        &mut restored,
        &[(&created, EventFlag::ItemCreated | EventFlag::ItemIsFile)],
    );
    assert!(restored.node_index_for_path(&created).is_some());
}
