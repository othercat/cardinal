use super::*;
use query::{VolumeQuery, do_query};
use search_cancel::CancellationToken;
use std::process::Command;

fn fixture_mounts(label: &str) -> Vec<Mount> {
    discover()
        .unwrap()
        .into_iter()
        .filter(|m| {
            m.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(label)
        })
        .collect()
}

fn rows(source: &Volume, query: &str, sort: Option<SortStatePayload>) -> Vec<serde_json::Value> {
    scoped_rows(source, query, sort, &[])
}

fn scoped_rows(
    source: &Volume,
    query: &str,
    sort: Option<SortStatePayload>,
    excluded: &[&str],
) -> Vec<serde_json::Value> {
    let request = VolumeQuery {
        query: Some(query.into()),
        directory_query: None,
        case_sensitive: false,
        sort,
        offset: 0,
        limit: 256,
        request_id: 1,
        excluded_paths: excluded.iter().map(PathBuf::from).collect(),
    };
    let reply = do_query(source, request, CancellationToken::noop()).unwrap();
    serde_json::to_value(reply).unwrap()["rows"]
        .as_array()
        .unwrap()
        .clone()
}
use crate::sort::SortStatePayload;
#[track_caller]
fn wait(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for volume state"
        );
        thread::sleep(Duration::from_millis(75));
    }
}
fn hdi(args: &[&OsStr]) {
    let out = Command::new("/usr/bin/hdiutil")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "hdiutil: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
struct Image {
    file: PathBuf,
    path: Option<PathBuf>,
}
impl Image {
    fn new(dir: &Path, file: &str, fs_type: &str, label: &str) -> Self {
        let file = dir.join(file);
        hdi(&[
            "create".as_ref(),
            "-size".as_ref(),
            "96m".as_ref(),
            "-fs".as_ref(),
            fs_type.as_ref(),
            "-volname".as_ref(),
            label.as_ref(),
            file.as_os_str(),
        ]);
        Self { file, path: None }
    }
    fn attach(&mut self, path: &Path) {
        assert!(!path.exists());
        hdi(&[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-mountpoint".as_ref(),
            path.as_os_str(),
            self.file.as_os_str(),
        ]);
        self.path = Some(path.into());
    }
    fn detach(&mut self) {
        if let Some(path) = self.path.take() {
            hdi(&["detach".as_ref(), path.as_os_str()]);
        }
    }
}
impl Drop for Image {
    fn drop(&mut self) {
        self.detach();
    }
}

#[test]
#[ignore = "mounts two isolated macOS disk images"]
fn mounted_disk_images_search_updates_and_offline_restart() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("cardinal_lib::volumes=trace")
        .try_init();
    let parent =
        PathBuf::from(std::env::var_os("CARDINAL_VOLUME_TEST_DIR").expect("isolated QA directory"));
    let label = format!("CardinalR5Test{}", std::process::id());
    let work = parent.join(&label);
    fs::create_dir_all(&work).unwrap();
    let cache_dir = work.join("volumes");
    let mount = Path::new("/Volumes").join(&label);
    let renamed = PathBuf::from(format!("{}-renamed", mount.display()));
    let mut a = Image::new(&work, "first.dmg", "JHFS+", &label);
    let mut b = Image::new(&work, "second.dmg", "APFS", &label);
    a.attach(&mount);
    fs::write(mount.join("needle-small.txt"), b"12").unwrap();
    fs::write(mount.join("needle-large.txt"), [1u8; 2048]).unwrap();
    let hot_file = mount.join("hot-metadata.tmp");
    fs::write(&hot_file, [0]).unwrap();
    let dir = mount.join("Nested");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("needle-delete.txt"), b"12345").unwrap();
    let store = Volumes::default();
    store.restore(&cache_dir).unwrap();
    let notify: Notify = Arc::new(|| {});
    let mounted = fixture_mounts(&label);
    assert_eq!(
        mounted.len(),
        1,
        "Disk Arbitration should discover the mounted test image"
    );
    let first = mounted[0].clone();
    assert!(first.persistent);
    let workers = store.reconcile(&cache_dir, mounted, &notify);
    let src = store.get(&first.id).unwrap();
    wait(|| !src.meta.lock().info.indexing);
    let sort = |direction| {
        Some(SortStatePayload {
            key: search_cache::SortKey::Size,
            direction,
        })
    };
    assert_eq!(
        rows(&src, "needle", sort(search_cache::SortDirection::Asc))
            .first()
            .unwrap()["metadata"]["size"],
        2
    );
    assert_eq!(
        rows(&src, "needle", sort(search_cache::SortDirection::Desc))
            .first()
            .unwrap()["metadata"]["size"],
        2048
    );
    fs::rename(
        mount.join("needle-small.txt"),
        dir.join("needle-renamed.txt"),
    )
    .unwrap();
    fs::write(dir.join("needle-live.txt"), b"live added").unwrap();
    fs::remove_file(dir.join("needle-delete.txt")).unwrap();
    wait(|| {
        rows(&src, "needle-live", None).len() == 1
            && rows(&src, "needle-delete", None).is_empty()
            && rows(&src, "needle-renamed", None).len() == 1
    });
    for size in [17, 159, 997] {
        fs::write(&hot_file, vec![0; size]).unwrap();
    }
    wait(|| rows(&src, "hot-metadata", None)[0]["metadata"]["size"] == 997);
    let t = Instant::now();
    a.detach();
    store.reconcile(&cache_dir, fixture_mounts(&label), &notify);
    for w in workers {
        w.join().unwrap();
    }
    assert!(!src.meta.lock().info.online);
    assert_eq!(rows(&src, "needle", None).len(), 3);
    assert_eq!(scoped_rows(&src, "needle", None, &["Nested"]).len(), 1);
    eprintln!("eject and final checkpoint: {:?}", t.elapsed());

    let store = Volumes::default();
    store.restore(&cache_dir).unwrap();
    let src = store.get(&first.id).unwrap();
    assert_eq!(rows(&src, "hot-metadata", None)[0]["metadata"]["size"], 997);
    let t = Instant::now();
    let saved = rows(&src, "needle", sort(search_cache::SortDirection::Desc));
    assert_eq!(saved.len(), 3);
    assert_eq!(saved[0]["metadata"]["size"], 2048);
    assert!(saved[0]["metadata"]["mtime"].as_u64().unwrap() > 0);
    eprintln!("offline restore/search/size-sort: {:?}", t.elapsed());

    b.attach(&mount);
    fs::write(mount.join("different-disk.txt"), b"second").unwrap();
    let mounted = fixture_mounts(&label);
    let second = mounted[0].clone();
    assert_ne!(second.id, first.id);
    assert!(!first.verified_path(&mount.join("needle-large.txt")));
    let workers = store.reconcile(&cache_dir, mounted, &notify);
    let src_b = store.get(&second.id).unwrap();
    wait(|| !src_b.meta.lock().info.indexing);
    assert_eq!(rows(&src_b, "different-disk", None).len(), 1);
    assert!(rows(&src_b, "needle", None).is_empty());
    assert!(rows(&src, "different-disk", None).is_empty());
    assert!(!src.meta.lock().info.online);
    b.detach();
    store.reconcile(&cache_dir, fixture_mounts(&label), &notify);
    for w in workers {
        w.join().unwrap();
    }

    // Simulate changes while Cardinal was closed and a different mount point on return.
    a.attach(&renamed);
    fs::write(renamed.join("while-away.txt"), b"new").unwrap();
    let workers = store.reconcile(&cache_dir, fixture_mounts(&label), &notify);
    wait(|| !src.meta.lock().info.indexing && rows(&src, "while-away", None).len() == 1);
    assert_eq!(scoped_rows(&src, "needle", None, &["Nested"]).len(), 1);
    assert!(
        rows(&src, "needle", None)
            .iter()
            .all(|v| Path::new(v["path"].as_str().unwrap()).starts_with(&renamed))
    );
    assert_eq!(store.list().len(), 2);
    a.detach();
    store.reconcile(&cache_dir, fixture_mounts(&label), &notify);
    for w in workers {
        w.join().unwrap();
    }
    for img in [&a, &b] {
        fs::remove_file(&img.file).unwrap();
    }
}

#[test]
fn volume_exclusions_filter_the_whole_order_and_undo_without_disk_access() {
    use fswalk::Node;
    fn node(name: &str, children: Vec<Node>) -> Node {
        Node {
            name: name.into(),
            metadata: None,
            children,
        }
    }
    let tree = node(
        "Drive",
        vec![
            node("cache", vec![node("hit-a", vec![])]),
            node("cache-old", vec![node("hit-b", vec![])]),
            node("hit-c", vec![]),
            node("hit-d", vec![]),
        ],
    );
    let mut index = Index::new(SearchCache::from_volume_tree(
        Path::new("/Volumes/Absent"),
        &tree,
        vec![],
        1,
        &APP_QUIT,
    ));
    let page = |index: &mut Index, paths: &[&str], offset| {
        let request = VolumeQuery {
            query: Some("hit-".into()),
            directory_query: None,
            case_sensitive: true,
            sort: Some(SortStatePayload {
                key: search_cache::SortKey::Filename,
                direction: search_cache::SortDirection::Asc,
            }),
            offset,
            limit: 1,
            request_id: 1,
            excluded_paths: paths.iter().map(PathBuf::from).collect(),
        };
        index
            .page(request, CancellationToken::noop())
            .map(|p| serde_json::to_value(p).unwrap())
    };
    let excluded = ["cache", "cache/hit-a", "hit-c"];
    let first = page(&mut index, &excluded, 0).unwrap();
    assert_eq!(first["total"], 2);
    assert_eq!(first["rows"][0]["path"], "/Volumes/Absent/cache-old/hit-b");
    assert_eq!(
        page(&mut index, &excluded, 1).unwrap()["rows"][0]["path"],
        "/Volumes/Absent/hit-d"
    );
    assert_eq!(page(&mut index, &[], 0).unwrap()["total"], 4);
    assert!(page(&mut index, &["../other"], 0).is_err());
    assert!(page(&mut index, &["/Volumes"], 0).is_err());
}

#[test]
fn offline_large_volume_sorts_and_pages_every_match_without_accessing_the_mount() {
    use fswalk::{Node, NodeFileType, NodeMetadata};
    use search_cache::{SortDirection, SortKey};
    use std::num::NonZeroU64;
    let n = 30_000usize;
    let file = |i: usize| Node {
        name: format!("item-{i:05}").into(),
        metadata: Some(NodeMetadata {
            r#type: NodeFileType::File,
            size: ((i * 19) % n) as u64,
            ctime: NonZeroU64::new(1 + ((i * 7) % n) as u64),
            mtime: NonZeroU64::new(1 + ((i * 11) % n) as u64),
        }),
        children: vec![],
    };
    let tree = Node {
        name: "Fixture".into(),
        metadata: None,
        children: (0..n).map(file).collect(),
    };
    let mount = Path::new("/Volumes/Cardinal-unmounted-fixture");
    let mut index = Index::new(SearchCache::from_volume_tree(
        mount,
        &tree,
        vec![],
        1,
        &APP_QUIT,
    ));
    for (key, attr) in [
        (SortKey::Size, "size"),
        (SortKey::Ctime, "ctime"),
        (SortKey::Mtime, "mtime"),
    ] {
        for direction in [SortDirection::Asc, SortDirection::Desc] {
            for offset in [0, 14_999, n - 7] {
                let request = VolumeQuery {
                    query: Some("item-".into()),
                    directory_query: None,
                    case_sensitive: false,
                    sort: Some(SortStatePayload { key, direction }),
                    offset,
                    limit: 256,
                    request_id: 1,
                    excluded_paths: vec![],
                };
                let json =
                    serde_json::to_value(index.page(request, CancellationToken::noop()).unwrap())
                        .unwrap();
                assert_eq!(json["total"], n);
                for (j, row) in json["rows"].as_array().unwrap().iter().enumerate() {
                    let rank = if direction == SortDirection::Asc {
                        offset + j
                    } else {
                        n - 1 - offset - j
                    };
                    assert_eq!(
                        row["metadata"][attr],
                        rank + usize::from(key != SortKey::Size)
                    );
                }
            }
        }
    }
}
