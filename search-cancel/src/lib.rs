use std::sync::atomic::{AtomicU64, Ordering};

/// How often long-running loops should check whether execution was cancelled.
pub const CANCEL_CHECK_INTERVAL: usize = 0x10000;

/// A global atomic identifies the active search version of Cardinal.
pub static ACTIVE_SEARCH_VERSION: AtomicU64 = AtomicU64::new(0);

/// A global atomic identifies the active scanning process version of Cardinal.
pub static ACTIVE_SCAN_VERSION: AtomicU64 = AtomicU64::new(0);

static ACTIVE_SORT_VERSION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub struct CancellationToken {
    active_version: &'static AtomicU64,
    version: u64,
}

impl CancellationToken {
    /// Admit a client-numbered request in an independent cancellation domain.
    /// Requests for pages of the same view share its version.
    pub fn for_request(active_version: &'static AtomicU64, version: u64) -> Self {
        active_version.fetch_max(version, Ordering::Relaxed);
        Self {
            active_version,
            version,
        }
    }

    pub fn noop() -> Self {
        static NOOP: AtomicU64 = AtomicU64::new(0);
        Self {
            version: 0,
            active_version: &NOOP,
        }
    }

    /// Creates a token for a search
    ///
    /// It increments the global search version and returns a token for
    /// that new version, so the caller does not need to specify one.
    pub fn new_search() -> Self {
        Self::cancel_sort();
        let version = self::ACTIVE_SEARCH_VERSION.fetch_add(1, Ordering::SeqCst) + 1;
        Self {
            version,
            active_version: &ACTIVE_SEARCH_VERSION,
        }
    }

    /// Background refreshes yield to user queries, and must not cancel a sort
    /// or a foreground query merely because a filesystem event arrived.
    pub fn background_search() -> Self {
        Self {
            version: ACTIVE_SEARCH_VERSION.load(Ordering::SeqCst),
            active_version: &ACTIVE_SEARCH_VERSION,
        }
    }

    pub fn new_scan() -> Self {
        Self::cancel_sort();
        let version = self::ACTIVE_SCAN_VERSION.fetch_add(1, Ordering::SeqCst) + 1;
        Self {
            version,
            active_version: &ACTIVE_SCAN_VERSION,
        }
    }

    pub fn is_cancelled(&self) -> Option<()> {
        if self.version != self.active_version.load(Ordering::Relaxed) {
            None
        } else {
            Some(())
        }
    }

    pub fn new_sort() -> Self {
        let version = ACTIVE_SORT_VERSION.fetch_add(1, Ordering::SeqCst) + 1;
        Self {
            version,
            active_version: &ACTIVE_SORT_VERSION,
        }
    }

    pub fn cancel_sort() {
        ACTIVE_SORT_VERSION.fetch_add(1, Ordering::SeqCst);
    }

    pub fn is_cancelled_sparse(&self, counter: usize) -> Option<()> {
        if counter.is_multiple_of(CANCEL_CHECK_INTERVAL) {
            self.is_cancelled()
        } else {
            Some(())
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    fn lock_versions() -> MutexGuard<'static, ()> {
        TEST_GUARD
            .lock()
            .expect("version lock should not be poisoned")
    }

    fn reset_versions() {
        ACTIVE_SEARCH_VERSION.store(0, Ordering::SeqCst);
        ACTIVE_SCAN_VERSION.store(0, Ordering::SeqCst);
        ACTIVE_SORT_VERSION.store(0, Ordering::SeqCst);
    }

    #[test]
    fn new_search_or_sort_cancels_previous_sort_without_cancelling_search() {
        let _guard = lock_versions();
        reset_versions();
        let search = CancellationToken::new_search();
        let first_sort = CancellationToken::new_sort();
        let next_sort = CancellationToken::new_sort();
        assert!(search.is_cancelled().is_some());
        assert!(first_sort.is_cancelled().is_none());
        assert!(next_sort.is_cancelled().is_some());
        let _ = CancellationToken::new_search();
        assert!(next_sort.is_cancelled().is_none());
    }

    #[test]
    fn background_refresh_yields_to_user_queries_without_cancelling_sort() {
        let _guard = lock_versions();
        reset_versions();
        let query = CancellationToken::new_search();
        let sort = CancellationToken::new_sort();
        let refresh = CancellationToken::background_search();
        assert!(query.is_cancelled().is_some());
        assert!(sort.is_cancelled().is_some());
        assert!(refresh.is_cancelled().is_some());
        let _ = CancellationToken::new_search();
        assert!(refresh.is_cancelled().is_none());
    }

    #[test]
    fn noop_token_is_never_cancelled() {
        let _guard = lock_versions();
        reset_versions();
        let token = CancellationToken::noop();
        assert!(
            token.is_cancelled().is_some(),
            "noop token should never be cancelled"
        );
    }

    #[test]
    fn search_token_cancelled_after_new_search_version() {
        let _guard = lock_versions();
        reset_versions();

        let search_v1 = CancellationToken::new_search();
        assert!(
            search_v1.is_cancelled().is_some(),
            "latest search token should start active"
        );

        let search_v2 = CancellationToken::new_search();
        assert!(
            search_v2.is_cancelled().is_some(),
            "new search token should be active"
        );
        assert!(
            search_v1.is_cancelled().is_none(),
            "older search token should be cancelled by newer search"
        );
    }

    #[test]
    fn cancelled_after_version_change() {
        let _guard = lock_versions();
        reset_versions();
        let token_v1 = CancellationToken::new_search();
        assert!(
            token_v1.is_cancelled().is_some(),
            "initial version should be active"
        );

        // Bump the active version, cancelling the older token.
        let _token_v2 = CancellationToken::new_search();
        assert!(token_v1.is_cancelled().is_none());
    }

    #[test]
    fn scan_token_cancelled_after_new_scan_version() {
        let _guard = lock_versions();
        reset_versions();

        let scan_v1 = CancellationToken::new_scan();
        assert!(
            scan_v1.is_cancelled().is_some(),
            "latest scan token should start active"
        );

        let scan_v2 = CancellationToken::new_scan();
        assert!(
            scan_v2.is_cancelled().is_some(),
            "new scan token should be active"
        );
        assert!(
            scan_v1.is_cancelled().is_none(),
            "older scan token should be cancelled by newer scan"
        );
    }

    #[test]
    fn scan_versions_do_not_cancel_search_tokens() {
        let _guard = lock_versions();
        reset_versions();

        let search_v1 = CancellationToken::new_search();
        let _scan_v1 = CancellationToken::new_scan();
        assert!(
            search_v1.is_cancelled().is_some(),
            "scan token updates should not affect search version"
        );

        let _search_v2 = CancellationToken::new_search();
        assert!(
            search_v1.is_cancelled().is_none(),
            "search token should still be governed by search version updates"
        );
    }
}
