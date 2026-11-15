use cardinal_sdk::{EventFlag, EventWatcher};
use search_cache::{SearchCache, SearchOptions, SlabIndex, SortDirection, SortKey};
use search_cancel::CancellationToken;
use std::{
    fs,
    io::Write,
    time::{Duration, Instant},
};

fn results(cache: &mut SearchCache) -> Vec<SlabIndex> {
    cache
        .search_with_options(
            "cardinal-live-",
            SearchOptions::default(),
            CancellationToken::noop(),
        )
        .unwrap()
        .nodes
        .unwrap()
}

fn wait_for(
    cache: &mut SearchCache,
    watcher: &EventWatcher,
    mut condition: impl FnMut(&mut SearchCache) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition(cache) {
            return;
        }
        if let Ok(events) = watcher.recv_timeout(Duration::from_millis(100)) {
            let diagnostic = format!("{events:?}");
            cache
                .handle_fs_events(events)
                .unwrap_or_else(|error| panic!("{error:?}: {diagnostic}"));
        }
    }
    panic!("FSEvents did not update the search index within 10 seconds");
}

#[test]
fn real_fsevents_create_modify_rename_delete_update_sorted_results() {
    let temp = tempdir::TempDir::new("cardinal_live_sort").unwrap();
    let root = temp.path().canonicalize().unwrap();
    let files = root.join("files");
    fs::create_dir(&files).unwrap();
    let mut cache = SearchCache::walk_fs(&root);
    let (_, watcher) = EventWatcher::spawn(
        root.to_string_lossy().into_owned(),
        cache.last_event_id(),
        0.05,
        vec![].into_boxed_slice(),
        vec![].into_boxed_slice(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "watcher history did not finish");
        if let Ok(events) = watcher.recv_timeout(Duration::from_millis(100)) {
            let ready = events
                .iter()
                .any(|event| event.flag.contains(EventFlag::HistoryDone));
            // Root creation can be replayed during startup and deliberately
            // requests a rescan. Rebuild at the history boundary below; this
            // test exercises subsequent live changes in a child directory.
            if ready {
                break;
            }
        }
    }

    // macOS can coalesce the just-created fixture root into a later batch.
    // Drain startup-only events before measuring incremental child changes.
    while watcher.recv_timeout(Duration::from_millis(500)).is_ok() {
        assert!(Instant::now() < deadline, "startup events did not settle");
    }
    cache = SearchCache::walk_fs(&root);

    let a = files.join("cardinal-live-a");
    let b = files.join("cardinal-live-b");
    let c = files.join("cardinal-live-c");
    fs::File::create(&a).unwrap().set_len(1 << 40).unwrap();
    fs::write(&b, vec![b'b'; 64 * 1024]).unwrap();
    wait_for(&mut cache, &watcher, |cache| results(cache).len() == 2);
    let ids = results(&mut cache);
    let sorted = cache
        .prepare_sort(&ids, SortKey::Size, CancellationToken::noop())
        .unwrap()
        .sort(SortDirection::Asc, CancellationToken::noop())
        .unwrap();
    assert_eq!(cache.node_path(sorted[0]).unwrap(), a);

    // Allocate blocks within the hole. The logical length and node ID stay fixed.
    fs::OpenOptions::new()
        .write(true)
        .open(&a)
        .unwrap()
        .write_all(&vec![b'a'; 128 * 1024])
        .unwrap();
    assert_eq!(fs::metadata(&a).unwrap().len(), 1 << 40);
    wait_for(&mut cache, &watcher, |cache| {
        let sorted = cache
            .prepare_sort(&ids, SortKey::Size, CancellationToken::noop())
            .unwrap()
            .sort(SortDirection::Asc, CancellationToken::noop())
            .unwrap();
        cache.node_path(sorted[0]).as_ref() == Some(&b)
    });
    fs::rename(&b, &c).unwrap();
    wait_for(&mut cache, &watcher, |cache| {
        results(cache)
            .iter()
            .any(|&id| cache.node_path(id).as_ref() == Some(&c))
    });
    fs::remove_file(&a).unwrap();
    wait_for(&mut cache, &watcher, |cache| results(cache).len() == 1);
    let remaining = results(&mut cache);
    assert_eq!(cache.node_path(remaining[0]).unwrap(), c);
}
