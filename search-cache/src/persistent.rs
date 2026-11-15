use crate::{
    SlabIndex, SlabNode, ThinSlab, name_index::SortedSlabIndices, slab_node::LegacyNodeMetadata,
};
use anyhow::{Context, Result, bail, ensure};
use hashbrown::HashSet;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread::available_parallelism,
    time::Instant,
};
use tracing::info;
use typed_num::Num;

pub const LSF_VERSION: i64 = 7;

#[derive(Serialize, Deserialize)]
pub struct PersistentStorage {
    pub version: Num<LSF_VERSION>,
    /// The last event id of the cache.
    pub last_event_id: u64,
    /// Root file path of the cache
    pub path: PathBuf,
    /// Ignore paths
    pub ignore_paths: Vec<PathBuf>,
    /// Paths to include even when they fall under an ignored directory.
    pub include_paths: Vec<PathBuf>,
    /// Root index of the slab
    pub slab_root: SlabIndex,
    pub slab: ThinSlab<SlabNode>,
    pub name_index: BTreeMap<Box<str>, SortedSlabIndices>,
    /// The number of rescans emitted before this snapshot.
    pub rescan_count: u64,
}

// v5 and v6 share this body. v6 added include_paths to the header.
// v7 adds allocated size to each node, preserving legacy paths, IDs and cursors.
#[derive(Deserialize)]
struct LegacyBody {
    slab_root: SlabIndex,
    #[serde(deserialize_with = "ThinSlab::deserialize_from::<SlabNode<LegacyNodeMetadata>, _>")]
    slab: ThinSlab<SlabNode>,
    name_index: BTreeMap<Box<str>, SortedSlabIndices>,
    rescan_count: u64,
}

/// Configuration probe only: callers must still load and validate the complete
/// snapshot before using it. Probing a mismatched configuration does not need
/// to allocate millions of nodes.
pub struct SnapshotHeader {
    pub source_version: i64,
    pub last_event_id: u64,
    pub path: PathBuf,
    pub ignore_paths: Vec<PathBuf>,
    pub include_paths: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct HeaderWithIncludes<const V: i64> {
    _version: Num<V>,
    last_event_id: u64,
    path: PathBuf,
    ignore_paths: Vec<PathBuf>,
    include_paths: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct HeaderV5 {
    _version: Num<5>,
    last_event_id: u64,
    path: PathBuf,
    ignore_paths: Vec<PathBuf>,
}

impl<const V: i64> From<HeaderWithIncludes<V>> for SnapshotHeader {
    fn from(header: HeaderWithIncludes<V>) -> Self {
        Self {
            source_version: V,
            last_event_id: header.last_event_id,
            path: header.path,
            ignore_paths: header.ignore_paths,
            include_paths: header.include_paths,
        }
    }
}

impl From<HeaderV5> for SnapshotHeader {
    fn from(header: HeaderV5) -> Self {
        Self {
            source_version: 5,
            last_event_id: header.last_event_id,
            path: header.path,
            ignore_paths: header.ignore_paths,
            include_paths: vec![],
        }
    }
}

impl SnapshotHeader {
    pub fn matches_configuration(
        &self,
        path: &Path,
        ignore: &[PathBuf],
        include: &[PathBuf],
    ) -> bool {
        self.path == path
            && same_paths(&self.ignore_paths, ignore)
            && same_paths(&self.include_paths, include)
    }
}

fn same_paths(left: &[PathBuf], right: &[PathBuf]) -> bool {
    // Longest-prefix include/ignore rules are sets: ordering and duplicate
    // entries do not change their semantics. Do not canonicalize filesystem
    // paths or weaken which subtree a snapshot is allowed to represent.
    left.iter().all(|path| right.contains(path)) && right.iter().all(|path| left.contains(path))
}

struct CancellableRead<'a, R> {
    inner: R,
    cancel: Option<&'a AtomicBool>,
}

impl<R: Read> Read for CancellableRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self
            .cancel
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            // Interrupted is retried by read_exact; a cancelled load must stop.
            return Err(io::Error::other("snapshot load cancelled"));
        }
        self.inner.read(buffer)
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    path: &Path,
    complete: bool,
    cancel: Option<&AtomicBool>,
) -> Result<T> {
    let input = File::open(path).context("Failed to open cache file")?;
    let input = CancellableRead {
        inner: input,
        cancel,
    };
    let decoder = zstd::Decoder::new(input).context("Failed to create zstd decoder")?;
    let mut input = BufReader::new(decoder);
    let mut bytes = vec![0u8; 4 * 1024];
    let value = postcard::from_io((&mut input, &mut bytes))
        .context("Failed to decode snapshot")?
        .0;
    if complete {
        // Deserializing the last field is not enough: drain to the real zstd
        // EOF so truncation/checksum errors and trailing payloads are rejected.
        let mut extra = [0];
        ensure!(
            input
                .read(&mut extra)
                .context("Incomplete snapshot frame")?
                == 0,
            "Snapshot contains trailing data"
        );
    }
    Ok(value)
}

pub fn read_snapshot_header(path: &Path) -> Result<SnapshotHeader> {
    read_snapshot_header_with_cancel(path, None)
}

pub(crate) fn read_snapshot_header_with_cancel(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<SnapshotHeader> {
    let version: i64 = decode(path, false, cancel)?;
    match version {
        7 => decode::<HeaderWithIncludes<7>>(path, false, cancel).map(Into::into),
        6 => decode::<HeaderWithIncludes<6>>(path, false, cancel).map(Into::into),
        5 => decode::<HeaderV5>(path, false, cancel).map(Into::into),
        _ => bail!("Unsupported snapshot version: {version}"),
    }
}

fn read_legacy<H: serde::de::DeserializeOwned + Into<SnapshotHeader>>(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<PersistentStorage> {
    let (header, old): (H, LegacyBody) = decode(path, true, cancel)?;
    let header = header.into();
    Ok(PersistentStorage {
        version: Num,
        last_event_id: header.last_event_id,
        path: header.path,
        ignore_paths: header.ignore_paths,
        include_paths: header.include_paths,
        slab_root: old.slab_root,
        slab: old.slab,
        name_index: old.name_index,
        rescan_count: old.rescan_count,
    })
}

pub fn read_cache_from_file(path: &Path) -> Result<PersistentStorage> {
    read_cache_from_file_with_cancel(path, None)
}

pub(crate) fn read_cache_from_file_with_cancel(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<PersistentStorage> {
    let cache_decode_time = Instant::now();
    let version: i64 = decode(path, false, cancel)?;
    let storage = match version {
        7 => decode(path, true, cancel)?,
        6 => read_legacy::<HeaderWithIncludes<6>>(path, cancel)?,
        5 => read_legacy::<HeaderV5>(path, cancel)?,
        _ => bail!("Unsupported snapshot version: {version}"),
    };
    let validation_time = Instant::now();
    validate(&storage, cancel)?;
    info!(
        "Cache structural validation time: {:?}",
        validation_time.elapsed()
    );
    info!("Cache decode time: {:?}", cache_decode_time.elapsed());
    Ok(storage)
}

fn validate(storage: &PersistentStorage, cancel: Option<&AtomicBool>) -> Result<()> {
    let root = storage
        .slab
        .get(storage.slab_root)
        .context("Missing snapshot root")?;
    ensure!(root.parent().is_none(), "Snapshot root has a parent");
    let mut remaining = HashSet::with_capacity(storage.slab.len());
    let mut stack = vec![storage.slab_root];
    while let Some(index) = stack.pop() {
        if remaining.len().is_multiple_of(8192) {
            ensure!(
                !cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)),
                "Snapshot load cancelled"
            );
        }
        ensure!(
            remaining.insert(index),
            "Snapshot tree contains duplicate/cyclic references"
        );
        let node = storage.slab.get(index).context("Dangling snapshot node")?;
        if index != storage.slab_root {
            ensure!(
                !node.name().is_empty()
                    && node.name() != "."
                    && node.name() != ".."
                    && !node.name().contains(['/', '\0']),
                "Snapshot contains an invalid path component"
            );
        }
        for &child in &node.children {
            ensure!(
                storage
                    .slab
                    .get(child)
                    .is_some_and(|node| node.parent() == Some(index)),
                "Snapshot child/parent references disagree"
            );
            stack.push(child);
        }
    }
    ensure!(
        remaining.len() == storage.slab.len(),
        "Snapshot contains unreachable nodes"
    );
    for (name, indices) in &storage.name_index {
        for &index in indices.iter() {
            if remaining.len().is_multiple_of(8192) {
                ensure!(
                    !cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)),
                    "Snapshot load cancelled"
                );
            }
            ensure!(
                storage
                    .slab
                    .get(index)
                    .is_some_and(|node| node.name() == name.as_ref()),
                "Snapshot name index references the wrong node"
            );
            ensure!(
                remaining.remove(&index),
                "Snapshot name index contains duplicate nodes"
            );
        }
    }
    ensure!(remaining.is_empty(), "Snapshot name index is incomplete");
    Ok(())
}

fn encode<W: Write>(output: W, storage: &PersistentStorage) -> Result<W> {
    let mut encoder = zstd::Encoder::new(output, 6).context("Failed to create zstd encoder")?;
    encoder
        .include_checksum(true)
        .context("Failed to enable snapshot checksum")?;
    encoder
        .multithread(available_parallelism().map(|n| n.get() as u32).unwrap_or(4))
        .context("Failed to create parallel zstd encoder")?;
    let mut buffered = BufWriter::with_capacity(256 * 1024, encoder);
    postcard::to_io(storage, &mut buffered).context("Failed to encode cache")?;
    let encoder = buffered
        .into_inner()
        .map_err(|error| error.into_error())
        .context("Failed to flush cache encoding")?;
    let mut output = encoder
        .finish()
        .context("Failed to finish cache compression")?;
    output.flush().context("Failed to flush encoded snapshot")?;
    Ok(output)
}

struct TemporarySnapshot(Option<PathBuf>);

impl Drop for TemporarySnapshot {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

// Keep the first pre-upgrade checkpoint for a downgrade. Local APFS/HFS+
// snapshots use a hard link; custom CLI destinations can lack hard-link support.
fn keep_legacy_snapshot(path: &Path, version: i64) -> Result<()> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".v{version}.bak"));
    let backup = PathBuf::from(name);
    let result = match fs::hard_link(path, &backup) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(_) => {
            let mut temporary =
                tempfile::NamedTempFile::new_in(backup.parent().unwrap_or(Path::new(".")))?;
            io::copy(&mut File::open(path)?, &mut temporary)?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(&backup)
                .map(|_| ())
                .map_err(|e| e.error)
        }
    };
    if let Err(error) = result {
        ensure!(
            error.kind() == io::ErrorKind::AlreadyExists
                && fs::symlink_metadata(&backup).is_ok_and(|m| m.is_file())
                && read_snapshot_header(&backup).is_ok_and(|h| h.source_version == version),
            "Cannot preserve pre-upgrade snapshot at {}: {error}",
            backup.display()
        );
    }
    Ok(())
}

pub fn write_cache_to_file(path: &Path, storage: &PersistentStorage) -> Result<()> {
    let cache_encode_time = Instant::now();
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).context("Failed to create snapshot directory")?;
    ensure!(
        !fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()),
        "Refusing to replace a symlink snapshot"
    );
    if let Ok(version) = decode::<i64>(path, false, None) {
        ensure!(
            (5..=LSF_VERSION).contains(&version),
            "Refusing to replace an unsupported snapshot version: {version}"
        );
        if version < LSF_VERSION {
            keep_legacy_snapshot(path, version)?;
        }
    }
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let filename = path.file_name().context("Snapshot path has no filename")?;
    let (output, temp_path) = loop {
        let mut name = filename.to_os_string();
        name.push(format!(
            ".{}.{}.sctmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(output) => break (output, temporary),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("Failed to create temporary snapshot"),
        }
    };
    let mut temporary = TemporarySnapshot(Some(temp_path.clone()));
    let output = encode(output, storage)?;
    output.sync_all().context("Failed to sync snapshot data")?;
    drop(output);
    fs::rename(&temp_path, path).context("Failed to atomically replace snapshot")?;
    temporary.0 = None;
    File::open(parent)?
        .sync_all()
        .context("Failed to sync snapshot directory")?;
    info!("Cache encode time: {:?}", cache_encode_time.elapsed());
    info!(
        "Cache size: {} MB",
        fs::symlink_metadata(path)
            .context("Failed to get cache file metadata")?
            .len() as f32
            / 1024.
            / 1024.
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventObservation, SearchCache, SlabNodeMetadataCompact};
    use cardinal_sdk::{EventFlag, FsEvent};
    use search_cancel::CancellationToken;
    use tempdir::TempDir;

    static NEVER_STOPPED: AtomicBool = AtomicBool::new(false);

    fn fixture() -> PersistentStorage {
        let mut slab = ThinSlab::new();
        let root = slab.insert(SlabNode::new(None, "", SlabNodeMetadataCompact::none()));
        let folder = slab.insert(SlabNode::new(
            Some(root),
            "fixture",
            SlabNodeMetadataCompact::none(),
        ));
        let file = slab.insert(SlabNode::new(
            Some(folder),
            "file.txt",
            SlabNodeMetadataCompact::none(),
        ));
        slab[root].children.push(folder);
        slab[folder].children.push(file);
        PersistentStorage {
            version: Num,
            last_event_id: 1234,
            path: "/fixture".into(),
            ignore_paths: vec!["/fixture/ignored".into(), "/fixture/other".into()],
            include_paths: vec![],
            slab_root: root,
            slab,
            name_index: BTreeMap::from([
                ("".into(), SortedSlabIndices::new(root)),
                ("fixture".into(), SortedSlabIndices::new(folder)),
                ("file.txt".into(), SortedSlabIndices::new(file)),
            ]),
            rescan_count: 2,
        }
    }

    fn raw_snapshot(path: &Path, bytes: &[u8]) {
        fs::write(path, zstd::stream::encode_all(bytes, 1).unwrap()).unwrap();
    }

    #[test]
    fn current_roundtrip_and_configuration_are_checked_before_use() {
        let dir = TempDir::new("snapshot_current").unwrap();
        let path = dir.path().join("db");
        let original = fixture();
        write_cache_to_file(&path, &original).unwrap();
        let header = read_snapshot_header(&path).unwrap();
        assert_eq!(header.source_version, LSF_VERSION);
        assert!(header.matches_configuration(
            &original.path,
            &[
                original.ignore_paths[1].clone(),
                original.ignore_paths[0].clone(),
                original.ignore_paths[0].clone()
            ],
            &[]
        ));
        let restored = read_cache_from_file(&path).unwrap();
        assert_eq!(restored.slab.len(), 3);
        assert_eq!(restored.last_event_id, 1234);
        assert_eq!(restored.rescan_count, 2);
        let mut cache = SearchCache::try_read_persistent_cache(
            &original.path,
            &path,
            &original.ignore_paths,
            &original.include_paths,
            &NEVER_STOPPED,
        )
        .unwrap();
        let result = cache
            .query_files("file.txt", CancellationToken::noop())
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, Path::new("/fixture/file.txt"));
        assert!(
            SearchCache::try_read_persistent_cache(
                Path::new("/other"),
                &path,
                &original.ignore_paths,
                &[],
                &NEVER_STOPPED
            )
            .is_err()
        );
        assert!(
            SearchCache::try_read_persistent_cache(&original.path, &path, &[], &[], &NEVER_STOPPED)
                .is_err()
        );
        assert!(
            SearchCache::try_read_persistent_cache(
                &original.path,
                &path,
                &original.ignore_paths,
                &["/fixture/ignored/allowed".into()],
                &NEVER_STOPPED
            )
            .is_err()
        );
    }

    #[test]
    fn v5_and_v6_migrate_in_place_with_the_original_node_ids_and_unknown_disk_usage() {
        use crate::{OptionSlabIndex, StateTypeSize};
        use fswalk::NodeFileType;
        let dir = TempDir::new("legacy_allocation").unwrap();
        let mut original = fixture();
        original.include_paths.push("/fixture/ignored/keep".into());
        let mut rows = BTreeMap::new();
        for (id, node) in original.slab.iter() {
            let kind = if node.children.is_empty() {
                NodeFileType::File
            } else {
                NodeFileType::Dir
            };
            let old_metadata = (StateTypeSize::some(kind, 1 << 40), 100u32, 200u32);
            rows.insert(
                id.get(),
                (
                    (node.name(), OptionSlabIndex::from_option(node.parent())),
                    &node.children,
                    old_metadata,
                ),
            );
        }
        for version in [5i64, 6] {
            let path = dir.path().join(format!("v{version}"));
            let mut bytes = postcard::to_stdvec(&(
                version,
                original.last_event_id,
                &original.path,
                &original.ignore_paths,
            ))
            .unwrap();
            if version == 6 {
                bytes.extend(postcard::to_stdvec(&original.include_paths).unwrap());
            }
            bytes.extend(
                postcard::to_stdvec(&(
                    original.slab_root,
                    &rows,
                    &original.name_index,
                    original.rescan_count,
                ))
                .unwrap(),
            );
            raw_snapshot(&path, &bytes);
            assert_eq!(read_snapshot_header(&path).unwrap().source_version, version);
            let mut migrated = read_cache_from_file(&path).unwrap();
            assert_eq!(migrated.include_paths.is_empty(), version == 5);
            assert_eq!(migrated.slab.len(), 3);
            assert_eq!(migrated.last_event_id, original.last_event_id);
            let file = migrated.slab.get_mut(SlabIndex::new(2)).unwrap();
            assert_eq!(file.name(), "file.txt");
            assert!(file.metadata.needs_hydration(true));
            assert!(!file.metadata.needs_hydration(false));
            assert_eq!(file.metadata.as_ref().unwrap().allocated_size(), None);
            file.metadata.hydrate_path(&dir.path().join("unreadable"));
            assert!(!file.metadata.needs_hydration(true));
            assert_eq!(file.metadata.as_ref().unwrap().size(), 1 << 40);
            assert_eq!(file.metadata.as_ref().unwrap().mtime().unwrap().get(), 200);
            let sparse = dir.path().join("hole");
            File::create(&sparse).unwrap().set_len(1 << 40).unwrap();
            file.metadata.hydrate_path(&sparse);
            assert_eq!(file.metadata.as_ref().unwrap().allocated_size(), Some(0));
            write_cache_to_file(&path, &migrated).unwrap();
            let backup = dir.path().join(format!("v{version}.v{version}.bak"));
            assert_eq!(
                read_snapshot_header(&backup).unwrap().source_version,
                version
            );
            assert_eq!(
                zstd::stream::decode_all(File::open(backup).unwrap()).unwrap(),
                bytes
            );
            assert_eq!(
                read_snapshot_header(&path).unwrap().source_version,
                LSF_VERSION
            );
            let saved = read_cache_from_file(&path).unwrap();
            assert_eq!(
                saved.slab[SlabIndex::new(2)]
                    .metadata
                    .as_ref()
                    .unwrap()
                    .allocated_size(),
                Some(0)
            );
        }
    }

    #[test]
    fn truncated_checksum_trailing_payload_and_unknown_version_are_rejected() {
        let dir = TempDir::new("snapshot_invalid").unwrap();
        let path = dir.path().join("db");
        let original = fixture();
        write_cache_to_file(&path, &original).unwrap();
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(
            read_cache_from_file(&path).is_err(),
            "missing frame trailer"
        );
        let mut bad_checksum = bytes.clone();
        *bad_checksum.last_mut().unwrap() ^= 1;
        fs::write(&path, bad_checksum).unwrap();
        assert!(read_cache_from_file(&path).is_err(), "bad frame checksum");
        let mut payload = postcard::to_stdvec(&original).unwrap();
        payload.push(0);
        raw_snapshot(&path, &payload);
        assert!(
            read_cache_from_file(&path).is_err(),
            "extra postcard payload"
        );
        raw_snapshot(&path, &postcard::to_stdvec(&99i64).unwrap());
        assert!(read_snapshot_header(&path).is_err(), "unknown layout");
        let unknown = fs::read(&path).unwrap();
        assert!(write_cache_to_file(&path, &original).is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            unknown,
            "newer layouts stay intact"
        );
    }

    #[test]
    fn invalid_tree_or_name_index_cannot_become_an_active_cache() {
        let mut storage = fixture();
        storage.slab_root = SlabIndex::new(100);
        assert!(validate(&storage, None).is_err());
        let mut storage = fixture();
        storage.slab[SlabIndex::new(1)]
            .children
            .push(SlabIndex::new(100));
        assert!(validate(&storage, None).is_err());
        let mut storage = fixture();
        storage.slab[SlabIndex::new(1)]
            .children
            .push(SlabIndex::new(2));
        assert!(validate(&storage, None).is_err());
        let mut storage = fixture();
        storage.slab[SlabIndex::new(1)].children.clear();
        assert!(validate(&storage, None).is_err());
        let mut storage = fixture();
        storage.name_index.remove("file.txt");
        assert!(validate(&storage, None).is_err());
        let mut storage = fixture();
        storage
            .name_index
            .insert("wrong".into(), SortedSlabIndices::new(SlabIndex::new(2)));
        assert!(validate(&storage, None).is_err());
        let dir = TempDir::new("snapshot_bad_graph").unwrap();
        let path = dir.path().join("db");
        write_cache_to_file(&path, &storage).unwrap();
        assert!(read_cache_from_file(&path).is_err());
    }

    #[test]
    fn save_failures_preserve_existing_destination_and_remove_only_own_temp() {
        let dir = TempDir::new("snapshot_atomic").unwrap();
        let path = dir.path().join("db");
        write_cache_to_file(&path, &fixture()).unwrap();
        let original = fs::read(&path).unwrap();
        let link = dir.path().join("linked");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(write_cache_to_file(&link, &fixture()).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        let blocked = dir.path().join("directory");
        fs::create_dir(&blocked).unwrap();
        let unrelated = dir.path().join("directory.sctmp");
        fs::write(&unrelated, b"keep").unwrap();
        assert!(write_cache_to_file(&blocked, &fixture()).is_err());
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 4);
    }

    #[test]
    fn encoding_propagates_write_and_flush_errors() {
        struct Failing {
            fail_write: bool,
        }
        impl Write for Failing {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.fail_write {
                    Err(io::Error::other("write failure"))
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("flush failure"))
            }
        }
        assert!(encode(Failing { fail_write: true }, &fixture()).is_err());
        assert!(encode(Failing { fail_write: false }, &fixture()).is_err());
    }

    #[test]
    fn cancelled_load_does_not_activate_snapshot() {
        let dir = TempDir::new("snapshot_cancel").unwrap();
        let path = dir.path().join("db");
        write_cache_to_file(&path, &fixture()).unwrap();
        assert!(read_cache_from_file_with_cancel(&path, Some(&AtomicBool::new(true))).is_err());
    }

    #[test]
    fn interrupted_event_checkpoint_retains_cursor_and_replays_idempotently() {
        let dir = TempDir::new("snapshot_replay").unwrap();
        let corpus = dir.path().join("corpus");
        fs::create_dir(&corpus).unwrap();
        let file = corpus.join("replayed.txt");
        fs::write(&file, b"current content").unwrap();
        let path = dir.path().join("db");
        let mut cache = SearchCache::walk_fs(&corpus);
        let committed = cache.last_event_id();
        cache.apply_observation(EventObservation::Missing(file.clone()));
        assert_eq!(cache.last_event_id(), committed);
        cache.flush_snapshot_to_file(&path).unwrap();
        let mut restored =
            SearchCache::try_read_persistent_cache(&corpus, &path, &[], &[], &NEVER_STOPPED)
                .unwrap();
        assert_eq!(restored.last_event_id(), committed);
        for _ in 0..2 {
            restored
                .handle_fs_events(vec![FsEvent {
                    path: file.clone(),
                    id: committed + 1,
                    flag: EventFlag::ItemCreated,
                }])
                .unwrap();
        }
        assert_eq!(restored.last_event_id(), committed + 1);
        let result = restored
            .query_files("replayed.txt", CancellationToken::noop())
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].metadata.as_ref().unwrap().size(), 15);
        restored.flush_to_file(&path).unwrap();
        let final_cache =
            SearchCache::try_read_persistent_cache(&corpus, &path, &[], &[], &NEVER_STOPPED)
                .unwrap();
        assert_eq!(final_cache.last_event_id(), committed + 1);
    }
}
