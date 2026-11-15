//! HFS+ searchfs(2) enumerates the mounted volume's catalog in resumable
//! batches. Names are joined using each record's parent catalog ID. Only
//! directories need a lookup table, including parents whose records arrive late.

use super::{
    ALLOCSIZE, AttrList, CRTIME, DATALENGTH, FLAGS, MODTIME, NAME, OBJTYPE, i64_at, invalid_record,
    timestamp, u32_at,
};
use crate::{Node, NodeFileType, NodeMetadata};
use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_int, c_void},
    fs,
    io::{self, ErrorKind},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

const ROOT_CNID: u64 = 2;
const OBJTAG: u32 = 0x0010;
const VT_HFS: u32 = 16;
const FILEID: u32 = 0x0200_0000;
const PARENTID: u32 = 0x0400_0000;
const DIR_LINKCOUNT: u32 = 1;
const DIR_ENTRYCOUNT: u32 = 2;
const UF_COMPRESSED: u32 = 0x20;
const START: u32 = 1;
const MATCH_DIRS: u32 = 4;
const MATCH_FILES: u32 = 8;

// Darwin LP64 ABI from <sys/attr.h> and <sys/time.h>.
#[repr(C)]
struct Timeval {
    seconds: i64,
    micros: i32,
}

#[repr(C)]
struct SearchBlock {
    return_attrs: *mut AttrList,
    buffer: *mut u8,
    buffer_size: usize,
    max_matches: usize,
    time_limit: Timeval,
    lower: *const u32,
    lower_size: usize,
    upper: *const u32,
    upper_size: usize,
    search_attrs: AttrList,
}

#[repr(C)]
struct SearchState {
    flags: u32,
    layer: u32,
    private: [u8; 548],
}

unsafe extern "C" {
    fn searchfs(
        path: *const c_char,
        block: *mut SearchBlock,
        matches: *mut usize,
        script_code: u32,
        options: u32,
        state: *mut SearchState,
    ) -> c_int;
    fn getattrlist(
        path: *const c_char,
        attrs: *mut AttrList,
        buffer: *mut c_void,
        size: usize,
        options: u64,
    ) -> c_int;
}

fn attrs(commonattr: u32, dirattr: u32, fileattr: u32) -> AttrList {
    AttrList {
        bitmapcount: 5,
        reserved: 0,
        commonattr,
        volattr: 0,
        dirattr,
        fileattr,
        forkattr: 0,
    }
}

fn require_hfs(path: &CStr) -> io::Result<()> {
    let mut tag = [0u32; 2];
    // SAFETY: length + vtagtype are two u32 values and all buffers outlive the call.
    let rc = unsafe {
        getattrlist(
            path.as_ptr(),
            &mut attrs(OBJTAG, 0, 0),
            tag.as_mut_ptr().cast(),
            size_of_val(&tag),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if tag != [8, VT_HFS] {
        return Err(io::Error::new(ErrorKind::Unsupported, "volume is not HFS+"));
    }
    Ok(())
}

#[derive(Default)]
struct Directory {
    record: Option<(Node, usize)>,
    files: Vec<Node>,
    subdirs: Vec<u64>,
}

type Catalog = HashMap<u64, Directory>;

fn insert_record(catalog: &mut Catalog, bytes: &[u8]) -> io::Result<()> {
    // Attribute references are relative to their own start (byte 4). Darwin
    // packs scalars on four-byte boundaries, including the two 64-bit IDs.
    let start = 4 + i64::from(u32_at(bytes, 4)? as i32);
    let start = usize::try_from(start).map_err(|_| invalid_record())?;
    let end = start
        .checked_add(u32_at(bytes, 8)? as usize)
        .ok_or_else(invalid_record)?;
    let name = bytes
        .get(start..end)
        .and_then(|b| CStr::from_bytes_with_nul(b).ok())
        .and_then(|s| s.to_str().ok())
        .ok_or_else(invalid_record)?;
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(invalid_record());
    }
    let kind = match u32_at(bytes, 12)? {
        1 => NodeFileType::File,
        2 => NodeFileType::Dir,
        5 => NodeFileType::Symlink,
        _ => NodeFileType::Unknown,
    };
    let id = u64::try_from(i64_at(bytes, 52)?).map_err(|_| invalid_record())?;
    let parent = u64::try_from(i64_at(bytes, 60)?).map_err(|_| invalid_record())?;
    let root = id == ROOT_CNID;
    if id < ROOT_CNID
        || (parent < ROOT_CNID && !root)
        || (root && (parent != 1 || kind != NodeFileType::Dir))
    {
        return Err(invalid_record());
    }
    let dir = kind == NodeFileType::Dir;
    let fixed_end = if dir { 76 } else { 84 };
    if start < fixed_end {
        return Err(invalid_record());
    }
    let (allocated_size, size) = if dir {
        (0, 0)
    } else {
        (
            u64::try_from(i64_at(bytes, 68)?).map_err(|_| invalid_record())?,
            u64::try_from(i64_at(bytes, 76)?).map_err(|_| invalid_record())?,
        )
    };
    let flags = u32_at(bytes, 48)?;
    let node = Node {
        name: name.into(),
        metadata: if !dir && (flags & UF_COMPRESSED != 0 || kind == NodeFileType::Unknown) {
            None // lstat supplies the logical size for compressed and special files
        } else {
            Some(NodeMetadata {
                r#type: kind,
                size,
                allocated_size,
                ctime: timestamp(bytes, 16)?,
                mtime: timestamp(bytes, 32)?,
            })
        },
        children: Vec::new(),
    };
    if dir {
        if u32_at(bytes, 68)? > 1 {
            return Err(io::Error::new(
                ErrorKind::Unsupported,
                "HFS+ directory hard links require directory traversal",
            ));
        }
        let count = u32_at(bytes, 72)? as usize;
        if catalog
            .entry(id)
            .or_default()
            .record
            .replace((node, count))
            .is_some()
        {
            return Err(invalid_record());
        }
        if !root {
            catalog.entry(parent).or_default().subdirs.push(id);
        }
    } else {
        catalog.entry(parent).or_default().files.push(node);
    }
    Ok(())
}

fn take_directory(
    catalog: &mut Catalog,
    id: u64,
    parent: &Path,
    cancelled: &impl Fn() -> bool,
) -> io::Result<Option<Node>> {
    if cancelled() {
        return Ok(None);
    }
    let Directory {
        record,
        mut files,
        subdirs,
    } = catalog.remove(&id).ok_or_else(invalid_record)?;
    let (mut node, expected_count) = record.ok_or_else(invalid_record)?;
    let path = if id == ROOT_CNID {
        node.name = parent
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into();
        // A read-only journaled HFS+ mount can return its private journal
        // records via searchfs even though they are absent in the namespace.
        // An ordinary file with that name on an unjournaled disk stays visible.
        files.retain(|file| {
            !matches!(file.name.as_ref(), ".journal" | ".journal_info_block")
                || !fs::symlink_metadata(parent.join(file.name.as_ref()))
                    .is_err_and(|err| err.kind() == ErrorKind::NotFound)
        });
        parent.to_owned()
    } else {
        parent.join(node.name.as_ref())
    };
    // A catalog can encode deeper paths than Darwin's pathname-based walker
    // can open. Bound both recursion and returned path semantics to PATH_MAX.
    if path.as_os_str().len() >= 1024 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "HFS+ path exceeds PATH_MAX",
        ));
    }
    // HFS directories can be renamed after their children have been cataloged.
    // Reject disconnected/ambiguous trees and counts that the kernel's access
    // checks do not explain. A normal traversal can recover what is readable.
    let count = files.len() + subdirs.len();
    if count != expected_count
        && (count != 0
            || !matches!(
                fs::read_dir(&path), Err(err) if err.kind() == ErrorKind::PermissionDenied
            ))
    {
        return Err(invalid_record());
    }
    node.children = files;
    for file in &mut node.children {
        if cancelled() {
            return Ok(None);
        }
        if file.metadata.is_none() {
            file.metadata = Some(fs::symlink_metadata(path.join(file.name.as_ref()))?.into());
        }
    }
    for child in subdirs {
        let Some(subtree) = take_directory(catalog, child, &path, cancelled)? else {
            return Ok(None);
        };
        node.children.push(subtree);
    }
    node.children.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    if node.children.windows(2).any(|p| p[0].name == p[1].name) {
        return Err(invalid_record());
    }
    Ok(Some(node))
}

/// Read a mounted HFS+ volume's complete visible tree (rooted at the mount
/// point) and its search metadata. Returns `Ok(None)` on cancellation. On an
/// unsupported, changing or inconsistent catalog the caller can use the normal
/// directory walker. No partial catalog escapes this function.
pub fn walk_hfs_catalog(root: &Path, cancelled: impl Fn() -> bool) -> io::Result<Option<Node>> {
    if cancelled() {
        return Ok(None);
    }
    let path = CString::new(root.as_os_str().as_bytes())?;
    let mount = fs::symlink_metadata(root)?;
    if !mount.is_dir() || mount.ino() != ROOT_CNID {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "expected the volume root",
        ));
    }
    require_hfs(&path)?;
    let mut catalog = Catalog::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let bounds = 4u32; // A length word and no search criteria matches every accessible item.
    let mut fields = attrs(
        NAME | OBJTYPE | CRTIME | MODTIME | FLAGS | FILEID | PARENTID,
        DIR_LINKCOUNT | DIR_ENTRYCOUNT,
        ALLOCSIZE | DATALENGTH,
    );
    let mut block = SearchBlock {
        return_attrs: &mut fields,
        buffer: buffer.as_mut_ptr(),
        buffer_size: buffer.len(),
        max_matches: 2048,
        time_limit: Timeval {
            seconds: 0,
            micros: 50_000,
        },
        lower: &bounds,
        lower_size: 4,
        upper: &bounds,
        upper_size: 4,
        search_attrs: attrs(0, 0, 0),
    };
    let mut state = SearchState {
        flags: 0,
        layer: 0,
        private: [0; 548],
    };
    let mut options = START | MATCH_DIRS | MATCH_FILES;
    loop {
        if cancelled() {
            return Ok(None);
        }
        let mut count = 0;
        // SAFETY: system-owned continuation state and all buffers match the
        // Darwin ABI. Nothing addressed by the block is reallocated across calls.
        let rc = unsafe {
            searchfs(
                path.as_ptr(),
                &mut block,
                &mut count,
                0x0800_0103,
                options,
                &mut state,
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.kind() != ErrorKind::WouldBlock {
                return Err(err);
            }
        }
        let mut rest = buffer.as_slice();
        for _ in 0..count {
            let len = u32_at(rest, 0)? as usize;
            let record = rest.get(..len).ok_or_else(invalid_record)?;
            insert_record(&mut catalog, record)?;
            rest = &rest[len..];
        }
        if rc == 0 {
            break;
        }
        options &= !START;
    }
    let Some(mut tree) = take_directory(&mut catalog, ROOT_CNID, root, &cancelled)? else {
        return Ok(None);
    };
    let after = fs::symlink_metadata(root)?;
    if !catalog.is_empty() || after.dev() != mount.dev() || after.ino() != mount.ino() {
        return Err(invalid_record());
    }
    tree.metadata = Some(after.into());
    Ok(Some(tree))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WalkData, walk_it_without_root_chain};
    use std::{cell::Cell, time::Instant};

    #[test]
    fn cancel_without_probing_and_reject_non_volume_roots() {
        assert!(
            walk_hfs_catalog(Path::new("/absent"), || true)
                .unwrap()
                .is_none()
        );
        let dir = tempdir::TempDir::new("not-a-volume").unwrap();
        assert_eq!(
            walk_hfs_catalog(dir.path(), || false).unwrap_err().kind(),
            ErrorKind::Unsupported
        );
    }

    #[test]
    fn reject_untrusted_catalog_records_before_attaching_any_children() {
        let mut catalog = Catalog::new();
        let mut record = [0u8; 88];
        record[4..8].copy_from_slice(&80i32.to_ne_bytes());
        record[8..12].copy_from_slice(&4u32.to_ne_bytes());
        record[12..16].copy_from_slice(&2u32.to_ne_bytes());
        record[52..60].copy_from_slice(&2u64.to_ne_bytes());
        record[60..68].copy_from_slice(&1u64.to_ne_bytes());
        record[84..88].copy_from_slice(b"vol\0");
        assert!(insert_record(&mut catalog, &record[..72]).is_err());
        record[68..72].copy_from_slice(&2u32.to_ne_bytes());
        assert!(insert_record(&mut catalog, &record).is_err());
        record[68..72].copy_from_slice(&1u32.to_ne_bytes());
        insert_record(&mut catalog, &record).unwrap();
        assert!(insert_record(&mut catalog, &record).is_err());
        record[8..12].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(insert_record(&mut catalog, &record).is_err());
    }

    /// Read-only. Point the environment variable at a stable mounted HFS+
    /// fixture; include hard links, symlinks and enough rows for several pages.
    #[test]
    #[ignore = "requires a mounted HFS+ test volume"]
    fn mounted_catalog_matches_directory_walk() {
        let mount = std::env::var_os("CARDINAL_HFS_TEST_VOLUME").expect("HFS+ mount point");
        let root = Path::new(&mount);
        let start = Instant::now();
        let catalog = walk_hfs_catalog(root, || false).unwrap().unwrap();
        let catalog_elapsed = start.elapsed();
        let start = Instant::now();
        let walk = walk_it_without_root_chain(&WalkData::simple(root, true)).unwrap();
        let walk_elapsed = start.elapsed();
        let mut pairs = vec![(&catalog, &walk, root.to_path_buf())];
        let mut count = 0;
        while let Some((actual, expected, path)) = pairs.pop() {
            count += 1;
            assert_eq!(actual.name, expected.name);
            assert_eq!(
                actual.children.len(),
                expected.children.len(),
                "{}",
                actual.name
            );
            let metadata = |node: &Node| {
                node.metadata.map(|m| {
                    (
                        m.r#type,
                        if m.r#type == NodeFileType::Dir {
                            (0, 0)
                        } else {
                            (m.size, m.allocated_size)
                        },
                        m.ctime,
                        m.mtime,
                    )
                })
            };
            assert_eq!(metadata(actual), metadata(expected), "{}", actual.name);
            if let Some(m) = actual.metadata
                && m.r#type != NodeFileType::Dir
            {
                assert_eq!(
                    m.allocated_size,
                    fs::symlink_metadata(&path).unwrap().blocks() * 512,
                    "{}",
                    path.display(),
                );
            }
            pairs.extend(
                actual
                    .children
                    .iter()
                    .zip(&expected.children)
                    .map(|(a, e)| (a, e, path.join(a.name.as_ref()))),
            );
        }
        eprintln!(
            "{count} matching entries; searchfs {catalog_elapsed:?}, directory walk {walk_elapsed:?}"
        );
        let polls = Cell::new(0);
        assert!(
            walk_hfs_catalog(root, || {
                polls.set(polls.get() + 1);
                polls.get() > 2
            })
            .unwrap()
            .is_none()
        );
    }
}
