//! Snapshot ownership and shutdown handling. An imported snapshot is read-only;
//! all later checkpoints go to this application's own database path.
use anyhow::{Context, Result, ensure};
use crossbeam_channel::{Sender, bounded};
use search_cache::SearchCache;
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Once, atomic::AtomicBool},
    time::Duration,
};
use tracing::{info, warn};

fn official_candidate(db_path: &Path) -> Option<PathBuf> {
    let parent = db_path.parent()?;
    (db_path.file_name()? == "cardinal.db" && parent.file_name()? == "com.othercat.cardinal").then(
        || {
            parent
                .parent()
                .map(|base| base.join("com.cardinal.one/cardinal.db"))
        },
    )?
}

fn startup_candidate(db_path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(db_path) {
        Ok(_) => Ok(db_path.to_owned()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // Never change data sources when a Local snapshot already exists,
            // even if it is damaged or has a different watch configuration.
            let path = official_candidate(db_path).context("No Local snapshot")?;
            fs::metadata(&path).context("No compatible source snapshot candidate")?;
            Ok(path)
        }
        Err(error) => Err(error).context("Cannot inspect Local snapshot"),
    }
}

pub(crate) fn has_startup_snapshot(db_path: &Path) -> bool {
    startup_candidate(db_path).is_ok()
}

pub(crate) fn load_startup_cache(
    root: &Path,
    db_path: &Path,
    ignore: &[PathBuf],
    include: &[PathBuf],
    cancel: &'static AtomicBool,
) -> Result<SearchCache> {
    let candidate = startup_candidate(db_path)?;
    let mut cache =
        SearchCache::try_read_persistent_cache(root, &candidate, ignore, include, cancel)?;
    check_event_cursor(cache.last_event_id(), cardinal_sdk::current_event_id())?;
    if candidate != db_path {
        info!("Loaded compatible official snapshot read-only; future saves use Local storage");
    }
    Ok(cache)
}

fn check_event_cursor(saved: u64, current: u64) -> Result<()> {
    ensure!(
        saved <= current,
        "Snapshot event cursor belongs to a newer filesystem history"
    );
    Ok(())
}

pub(crate) fn flush_cache_to_file_once(
    finish_tx: &Sender<Sender<Option<SearchCache>>>,
    db_path: &Path,
) {
    static FLUSH_ONCE: Once = Once::new();
    FLUSH_ONCE.call_once(|| {
        // The index thread decides whether its tree is complete. Replaying
        // history is allowed: its cursor still marks the last complete batch.
        match take_and_save_cache(finish_tx, db_path, Duration::from_secs(10)) {
            Ok(true) => info!("Final snapshot saved"),
            Ok(false) => info!("Initial index is incomplete; kept the previous snapshot"),
            Err(error) => warn!("Final snapshot was not saved: {error:#}"),
        }
    });
}

fn take_and_save_cache(
    finish_tx: &Sender<Sender<Option<SearchCache>>>,
    db_path: &Path,
    timeout: Duration,
) -> Result<bool> {
    let (cache_tx, cache_rx) = bounded(1);
    finish_tx
        .send_timeout(cache_tx, timeout)
        .context("Index thread is unavailable")?;
    let cache = cache_rx
        .recv_timeout(timeout)
        .context("Index thread did not finish")?;
    if let Some(cache) = cache {
        cache
            .flush_to_file(db_path)
            .context("Cannot save final snapshot")?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static STOP: AtomicBool = AtomicBool::new(false);
    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cardinal-persistence-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn source_reuse_requires_missing_local_and_exact_application_identity() {
        let temp = TestDirectory::new();
        let corpus = temp.0.join("corpus");
        fs::create_dir(&corpus).unwrap();
        fs::write(corpus.join("fixture.txt"), b"fixture").unwrap();
        let local = temp.0.join("com.othercat.cardinal/cardinal.db");
        let official = temp.0.join("com.cardinal.one/cardinal.db");
        SearchCache::walk_fs(&corpus)
            .flush_to_file(&official)
            .unwrap();
        let source = fs::read(&official).unwrap();
        assert!(has_startup_snapshot(&local));
        let loaded = load_startup_cache(&corpus, &local, &[], &[], &STOP).unwrap();
        assert!(loaded.get_total_files() > 1);
        assert!(
            !local.exists(),
            "import is read-only until a normal checkpoint"
        );
        assert_eq!(fs::read(&official).unwrap(), source);
        assert!(!has_startup_snapshot(
            &temp.0.join("com.othercat.cardinal.qa/cardinal.db")
        ));
        assert!(load_startup_cache(&temp.0.join("another-root"), &local, &[], &[], &STOP).is_err());
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&local, b"damaged local").unwrap();
        assert!(load_startup_cache(&corpus, &local, &[], &[], &STOP).is_err());
        assert_eq!(fs::read(&local).unwrap(), b"damaged local");
        assert_eq!(fs::read(&official).unwrap(), source);
    }

    #[test]
    fn future_event_history_is_rejected() {
        assert!(check_event_cursor(10, 9).is_err());
        assert!(check_event_cursor(10, 10).is_ok());
        assert!(check_event_cursor(0, 10).is_ok());
    }

    #[test]
    fn shutdown_saves_complete_cache_and_leaves_incomplete_checkpoint_untouched() {
        let temp = TestDirectory::new();
        let corpus = temp.0.join("corpus");
        fs::create_dir(&corpus).unwrap();
        fs::write(corpus.join("fixture.txt"), b"fixture").unwrap();
        let db = temp.0.join("db");
        let (tx, rx) = bounded::<Sender<Option<SearchCache>>>(1);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                rx.recv()
                    .unwrap()
                    .send(Some(SearchCache::walk_fs(&corpus)))
                    .unwrap();
            });
            assert!(take_and_save_cache(&tx, &db, Duration::from_secs(1)).unwrap());
        });
        let previous = fs::read(&db).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                rx.recv().unwrap().send(None).unwrap();
            });
            assert!(!take_and_save_cache(&tx, &db, Duration::from_secs(1)).unwrap());
        });
        assert_eq!(fs::read(&db).unwrap(), previous);
        assert!(SearchCache::try_read_persistent_cache(&corpus, &db, &[], &[], &STOP).is_ok());
    }

    #[test]
    fn shutdown_channel_and_disk_errors_return_without_panicking() {
        let temp = TestDirectory::new();
        let db = temp.0.join("db");
        let (tx, rx) = bounded::<Sender<Option<SearchCache>>>(1);
        drop(rx);
        assert!(take_and_save_cache(&tx, &db, Duration::from_millis(10)).is_err());
        let (tx, _rx) = bounded::<Sender<Option<SearchCache>>>(1);
        assert!(take_and_save_cache(&tx, &db, Duration::from_millis(10)).is_err());
        let (tx, rx) = bounded::<Sender<Option<SearchCache>>>(1);
        fs::create_dir(&db).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                rx.recv()
                    .unwrap()
                    .send(Some(SearchCache::walk_fs(&temp.0)))
                    .unwrap();
            });
            assert!(take_and_save_cache(&tx, &db, Duration::from_secs(1)).is_err());
        });
    }
}
