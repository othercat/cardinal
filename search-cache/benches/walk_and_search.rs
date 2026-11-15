//! Benchmarks for `SearchCache` walk and query performance.
//!
//! Set `BENCH_CORPUS_DIR` to the root of a large directory tree (e.g. a shallow
//! clone of the Linux kernel)
//!
//! Run:
//!   BENCH_CORPUS_DIR=/path/to/linux cargo bench -p search-cache

use criterion::{BatchSize, BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};
use search_cache::{SearchCache, SortDirection, SortKey};
use search_cancel::CancellationToken;
use std::{path::PathBuf, time::Duration};

fn corpus() -> PathBuf {
    std::env::var("BENCH_CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/homebrew"))
}

// measures filesystem traversal + slab construction
fn bench_walk_fs(c: &mut Criterion) {
    let path = corpus();
    let mut group = c.benchmark_group("walk_fs");
    // Walking a large tree takes ~seconds; reduce sample count and extend
    // measurement time so Criterion doesn't time-out or flood the FS.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(60));
    group.sampling_mode(SamplingMode::Flat);

    group.bench_function("corpus", |b| {
        b.iter(|| SearchCache::walk_fs(&path));
    });

    group.finish();
}

const QUERIES: &[&str] = &[
    // Exact file name
    "Makefile",
    // Extension glob
    "*.rs",
    // Multi-segment (path + name)
    "src main",
    // Common header name
    "*.h",
    // Term that should match very few results
    "ffffffff_no_match_xyzzy",
];

// Measures search latency on a fresh cache per iteration. Cache construction is
// done outside the timed region so this isolates the query hot path.
fn bench_query_files(c: &mut Criterion) {
    let path = corpus();
    let mut group = c.benchmark_group("query_files");
    group.measurement_time(Duration::from_secs(10));

    for query in QUERIES {
        group.bench_with_input(BenchmarkId::new("query", query), query, |b, &q| {
            // Use `PerIteration` on purpose: the setup value is a full
            // `SearchCache`, not a small input. With `SmallInput` or
            // `LargeInput`, Criterion would hold multiple caches in memory at
            // once for a batch. `PerIteration` keeps only one cache alive at a
            // time, and Criterion still excludes setup from the timed region.
            b.iter_batched(
                || SearchCache::walk_fs(&path),
                |mut cache| cache.query_files(q, CancellationToken::noop()).ok(),
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

// measures search latency on an already-built cache
fn bench_query_files_warm(c: &mut Criterion) {
    let path = corpus();
    let mut cache = SearchCache::walk_fs(&path);

    let mut group = c.benchmark_group("query_files_warm");
    group.measurement_time(Duration::from_secs(10));

    for query in QUERIES {
        group.bench_with_input(BenchmarkId::new("query", query), query, |b, &q| {
            b.iter(|| cache.query_files(q, CancellationToken::noop()).ok());
        });
    }

    group.finish();
}

// Measures snapshot creation + cancellable sorting, excluding the initial walk.
// Numeric keys are warm after Criterion's warmup; cold metadata I/O is separate.
fn bench_indexed_sort(c: &mut Criterion) {
    let mut cache = SearchCache::walk_fs(&corpus());
    let indices = cache.search_empty(CancellationToken::noop()).unwrap();
    let mut group = c.benchmark_group("indexed_sort");
    for (label, key) in [
        ("filename", SortKey::Filename),
        ("full_path", SortKey::FullPath),
        ("size_warm", SortKey::Size),
        ("mtime_warm", SortKey::Mtime),
        ("ctime_warm", SortKey::Ctime),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                cache
                    .prepare_sort(&indices, key, CancellationToken::noop())
                    .unwrap()
                    .sort(SortDirection::Asc, CancellationToken::noop())
                    .unwrap()
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_walk_fs,
    bench_query_files,
    bench_query_files_warm,
    bench_indexed_sort
);
criterion_main!(benches);
