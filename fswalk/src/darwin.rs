//! Darwin's directory enumeration can return the search metadata in the same
//! syscall as the names. The ABI and packing rules come from <sys/attr.h> and
//! getattrlistbulk(2); unsupported volumes fall back to std::fs::read_dir.

use super::{NodeFileType, NodeMetadata};
use std::{
    cell::RefCell,
    ffi::{OsString, c_int, c_void},
    fs::OpenOptions,
    io::{self, ErrorKind},
    num::NonZeroU64,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStringExt, fs::OpenOptionsExt},
    },
    path::Path,
};

const NAME: u32 = 0x0000_0001;
const OBJTYPE: u32 = 0x0000_0008;
const CRTIME: u32 = 0x0000_0200;
const MODTIME: u32 = 0x0000_0400;
const FLAGS: u32 = 0x0004_0000;
const ERROR: u32 = 0x2000_0000;
const RETURNED: u32 = 0x8000_0000;
const DATALENGTH: u32 = 0x0000_0200;
const MOUNTSTATUS: u32 = 0x0000_0004;
const PACK_INVALID: u64 = 0x0000_0008;
const O_NOFOLLOW: i32 = 0x0000_0100;
const O_DIRECTORY: i32 = 0x0010_0000;
const SF_FIRMLINK: u32 = 0x0080_0000;

// The kernel fills this aligned scratch space on each read. Reuse one
// buffer per enumerator thread, including for trees with many tiny directories.
thread_local! { static SCRATCH: RefCell<Vec<u64>> = RefCell::new(vec![0; 8192]); }

#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(
        fd: c_int,
        attrs: *mut AttrList,
        buffer: *mut c_void,
        size: usize,
        options: u64,
    ) -> c_int;
}

pub(super) struct Entry {
    pub name: OsString,
    pub kind: NodeFileType,
    pub metadata: Option<NodeMetadata>,
}

/// None is cancellation. An error lets the caller restart this directory with
/// the portable reader; no partial batch or counters have been published yet.
pub(super) fn read_directory(
    path: &Path,
    metadata: bool,
    cancelled: impl Fn() -> bool,
) -> io::Result<Option<Vec<Entry>>> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_DIRECTORY)
        .open(path)?;
    let mut attrs = AttrList {
        bitmapcount: 5,
        reserved: 0,
        commonattr: NAME | OBJTYPE | ERROR | RETURNED,
        volattr: 0,
        dirattr: if metadata { MOUNTSTATUS } else { 0 },
        fileattr: if metadata { DATALENGTH } else { 0 },
        forkattr: 0,
    };
    if metadata {
        attrs.commonattr |= CRTIME | MODTIME | FLAGS;
    }
    SCRATCH.with(|scratch| {
        // A cancellation callback may itself enter a walker on this thread.
        if let Ok(mut buffer) = scratch.try_borrow_mut() {
            read_entries(&directory, &mut attrs, &mut buffer, metadata, cancelled)
        } else {
            read_entries(
                &directory,
                &mut attrs,
                &mut vec![0; 8192],
                metadata,
                cancelled,
            )
        }
    })
}

fn read_entries(
    directory: &std::fs::File,
    attrs: &mut AttrList,
    buffer: &mut [u64],
    metadata: bool,
    cancelled: impl Fn() -> bool,
) -> io::Result<Option<Vec<Entry>>> {
    let mut entries = Vec::new();
    loop {
        if cancelled() {
            return Ok(None);
        }
        // SAFETY: directory owns a readable directory fd; attrs has the Darwin
        // ABI layout, and the aligned buffer is writable for its entire length.
        let count = unsafe {
            getattrlistbulk(
                directory.as_raw_fd(),
                attrs,
                buffer.as_mut_ptr().cast(),
                buffer.len() * 8,
                PACK_INVALID,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Ok(Some(entries));
        }
        // SAFETY: the buffer is initialized bytes and remains alive/immutable
        // until every returned record has been checked and copied below.
        let bytes =
            unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), buffer.len() * 8) };
        let mut cursor = 0;
        for _ in 0..count {
            if cancelled() {
                return Ok(None);
            }
            let size = u32_at(bytes, cursor)? as usize;
            let end = cursor.checked_add(size).ok_or_else(invalid_record)?;
            let record = bytes.get(cursor..end).ok_or_else(invalid_record)?;
            if size < 40 {
                return Err(invalid_record());
            }
            let entry = parse_record(record, metadata)?;
            entries.push(entry);
            cursor = end;
        }
    }
}

fn invalid_record() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "invalid getattrlistbulk record")
}

fn u32_at(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let value = bytes.get(offset..offset + 4).ok_or_else(invalid_record)?;
    Ok(u32::from_ne_bytes(value.try_into().unwrap()))
}

fn i64_at(bytes: &[u8], offset: usize) -> io::Result<i64> {
    let value = bytes.get(offset..offset + 8).ok_or_else(invalid_record)?;
    Ok(i64::from_ne_bytes(value.try_into().unwrap()))
}

fn timestamp(bytes: &[u8], offset: usize) -> io::Result<Option<NonZeroU64>> {
    Ok(u64::try_from(i64_at(bytes, offset)?)
        .ok()
        .and_then(NonZeroU64::new))
}

fn parse_record(record: &[u8], metadata: bool) -> io::Result<Entry> {
    let common = u32_at(record, 4)?;
    let dir = u32_at(record, 12)?;
    let file = u32_at(record, 16)?;
    if common & (NAME | OBJTYPE) != NAME | OBJTYPE || u32_at(record, 24)? != 0 {
        // Restarting via std also preserves entries whose attributes could not
        // be read even though directory enumeration itself succeeded.
        return Err(invalid_record());
    }
    // ATTR_CMN_ERROR precedes NAME, independent of its bitmap position.
    let name_offset = i64::from(u32_at(record, 28)? as i32) + 28;
    let name_size = u32_at(record, 32)? as usize;
    let start = usize::try_from(name_offset).map_err(|_| invalid_record())?;
    let end = start.checked_add(name_size).ok_or_else(invalid_record)?;
    let name = record.get(start..end).ok_or_else(invalid_record)?;
    if name.len() < 2 || name.last() != Some(&0) {
        return Err(invalid_record());
    }
    let name = &name[..name.len() - 1];
    if name == b"." || name == b".." || name.contains(&b'/') || name.contains(&0) {
        return Err(invalid_record());
    }
    let kind = match u32_at(record, 36)? {
        1 => NodeFileType::File,
        2 => NodeFileType::Dir,
        5 => NodeFileType::Symlink,
        _ => NodeFileType::Unknown,
    };
    let metadata = if metadata {
        let flags = u32_at(record, 72)?;
        // Directory/file groups are type-specific even with PACK_INVALID:
        // directories contain MOUNTSTATUS here; other vnodes contain DATALENGTH.
        let (mount, size) = if kind == NodeFileType::Dir {
            (u32_at(record, 76)?, 0)
        } else {
            (0, i64_at(record, 76)?)
        };
        let complete = common & (CRTIME | MODTIME | FLAGS) == CRTIME | MODTIME | FLAGS;
        // Mount/firmlink attributes describe the underlying node, not the
        // mounted/firmlink target. Stat these directories when actually opened.
        // getattrlistbulk explicitly returns the link's own attributes, so it
        // supplies the same size/timestamps as lstat for regular files and links.
        let usable = complete
            && match kind {
                NodeFileType::File | NodeFileType::Symlink => file & DATALENGTH != 0 && size >= 0,
                NodeFileType::Dir => {
                    dir & MOUNTSTATUS != 0 && mount & 1 == 0 && flags & SF_FIRMLINK == 0
                }
                _ => false,
            };
        usable.then(|| NodeMetadata {
            r#type: kind,
            size: if kind != NodeFileType::Dir {
                size as u64
            } else {
                0
            },
            ctime: timestamp(record, 40).unwrap_or(None),
            mtime: timestamp(record, 56).unwrap_or(None),
        })
    } else {
        None
    };
    Ok(Entry {
        name: OsString::from_vec(name.to_vec()),
        kind,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_metadata_matches_lstat_and_never_opens_symlink_targets() {
        let temp = tempdir::TempDir::new("bulk_attributes").unwrap();
        let path = temp.path();
        std::fs::write(path.join("中文-é.txt"), b"metadata from enumeration").unwrap();
        std::fs::create_dir(path.join("empty-directory")).unwrap();
        std::os::unix::fs::symlink(path.join("empty-directory"), path.join("link")).unwrap();
        let entries = read_directory(path, true, || false).unwrap().unwrap();
        assert_eq!(entries.len(), 3);
        let file = entries
            .iter()
            .find(|entry| entry.name == "中文-é.txt")
            .unwrap();
        let actual = file
            .metadata
            .expect("APFS should vend the requested attributes");
        let expected =
            NodeMetadata::from(std::fs::symlink_metadata(path.join(&file.name)).unwrap());
        assert_eq!(actual.r#type, expected.r#type);
        assert_eq!(actual.size, expected.size);
        assert_eq!(actual.ctime, expected.ctime);
        assert_eq!(actual.mtime, expected.mtime);
        let directory = entries
            .iter()
            .find(|entry| entry.name == "empty-directory")
            .unwrap();
        assert_eq!(directory.metadata.unwrap().r#type, NodeFileType::Dir);
        let link = entries.iter().find(|entry| entry.name == "link").unwrap();
        assert_eq!(link.kind, NodeFileType::Symlink);
        let link_meta = link.metadata.unwrap();
        let expected = NodeMetadata::from(std::fs::symlink_metadata(path.join("link")).unwrap());
        assert_eq!(link_meta.size, expected.size);
        assert_eq!(link_meta.ctime, expected.ctime);
        assert_eq!(link_meta.mtime, expected.mtime);
        assert!(read_directory(&path.join("link"), true, || false).is_err());
    }

    #[test]
    fn bulk_reader_crosses_buffer_boundaries_without_duplicating_entries() {
        let temp = tempdir::TempDir::new("bulk_batches").unwrap();
        for i in 0..2000 {
            std::fs::File::create(temp.path().join(format!("entry-{i:05}"))).unwrap();
        }
        for metadata in [false, true] {
            let entries = read_directory(temp.path(), metadata, || false)
                .unwrap()
                .unwrap();
            assert_eq!(entries.len(), 2000);
            let names: std::collections::HashSet<_> =
                entries.iter().map(|entry| &entry.name).collect();
            assert_eq!(names.len(), 2000);
            assert!(
                entries
                    .iter()
                    .all(|entry| entry.metadata.is_some() == metadata)
            );
        }
        assert!(
            read_directory(temp.path(), true, || true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_truncated_or_out_of_bounds_attribute_records() {
        assert!(parse_record(&[0; 20], true).is_err());
        let mut bytes = vec![0u8; 96];
        bytes[4..8].copy_from_slice(&(NAME | OBJTYPE).to_ne_bytes());
        bytes[28..32].copy_from_slice(&1000u32.to_ne_bytes());
        bytes[32..36].copy_from_slice(&4u32.to_ne_bytes());
        assert!(parse_record(&bytes, false).is_err());
        bytes[28..32].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(parse_record(&bytes, false).is_err());
    }

    #[test]
    fn mount_and_firmlink_directory_metadata_requires_a_target_stat() {
        let mut bytes = vec![0u8; 88];
        bytes[4..8].copy_from_slice(&(NAME | OBJTYPE | CRTIME | MODTIME | FLAGS).to_ne_bytes());
        bytes[12..16].copy_from_slice(&MOUNTSTATUS.to_ne_bytes());
        bytes[28..32].copy_from_slice(&52u32.to_ne_bytes());
        bytes[32..36].copy_from_slice(&4u32.to_ne_bytes());
        bytes[36..40].copy_from_slice(&2u32.to_ne_bytes());
        bytes[80..84].copy_from_slice(b"dir\0");
        assert!(parse_record(&bytes, true).unwrap().metadata.is_some());
        bytes[76..80].copy_from_slice(&1u32.to_ne_bytes());
        assert!(parse_record(&bytes, true).unwrap().metadata.is_none());
        bytes[76..80].fill(0);
        bytes[72..76].copy_from_slice(&SF_FIRMLINK.to_ne_bytes());
        assert!(parse_record(&bytes, true).unwrap().metadata.is_none());
    }
}
