use crossbeam_channel::{Receiver, Sender, bounded};
use std::{
    collections::HashMap,
    ffi::{CStr, c_char, c_void},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::LazyLock,
};

type Visitor = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    i32,
    bool,
);
unsafe extern "C" {
    fn cardinal_visit_mounts(context: *mut c_void, callback: Visitor) -> bool;
    fn cardinal_watch_mounts(changed: extern "C" fn());
    fn cardinal_mount_paths(
        context: *mut c_void,
        visit: unsafe extern "C" fn(*mut c_void, *const c_char),
    );
    fn uuid_generate_random(out: *mut u8);
    fn uuid_unparse_lower(uuid: *const u8, out: *mut c_char);
}

pub(super) fn separate_snapshot_id() -> String {
    let mut bytes = [0; 16];
    let mut text = [0; 37];
    unsafe {
        uuid_generate_random(bytes.as_mut_ptr());
        uuid_unparse_lower(bytes.as_ptr(), text.as_mut_ptr());
        format!(
            "snapshot-{}",
            CStr::from_ptr(text.as_ptr()).to_string_lossy()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mount {
    pub id: String,
    pub path: PathBuf,
    pub format: String,
    pub journal: Option<String>,
    pub dev: i32,
    pub persistent: bool,
    pub read_only: bool,
    pub nested: Vec<PathBuf>,
}

pub(super) fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

pub(super) fn discover() -> Option<Vec<Mount>> {
    unsafe extern "C" fn visit(
        context: *mut c_void,
        path: *const c_char,
        format: *const c_char,
        id: *const c_char,
        journal: *const c_char,
        dev: i32,
        read_only: bool,
    ) {
        let identifier = |ptr: *const c_char| {
            (!ptr.is_null())
                .then(|| unsafe { CStr::from_ptr(ptr) }.to_str().ok())
                .flatten()
                .filter(|s| uuid(s))
                .map(str::to_ascii_lowercase)
        };
        // The Objective-C caller keeps the C strings alive for this callback only.
        let output = unsafe { &mut *context.cast::<Vec<Mount>>() };
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(
            unsafe { CStr::from_ptr(path) }.to_bytes(),
        ));
        let format = unsafe { CStr::from_ptr(format) }
            .to_string_lossy()
            .into_owned();
        if !format
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return;
        }
        let id = identifier(id);
        output.push(Mount {
            persistent: id.is_some(),
            read_only,
            id: id.unwrap_or_default(),
            path,
            format,
            journal: identifier(journal),
            dev,
            nested: vec![],
        });
    }
    let mut mounts = Vec::<Mount>::new();
    if !unsafe { cardinal_visit_mounts((&mut mounts as *mut Vec<Mount>).cast(), visit) } {
        return None;
    }
    unsafe extern "C" fn collect(ctx: *mut c_void, path: *const c_char) {
        let paths = unsafe { &mut *ctx.cast::<Vec<PathBuf>>() };
        paths.push(std::ffi::OsStr::from_bytes(unsafe { CStr::from_ptr(path) }.to_bytes()).into());
    }
    let mut paths = Vec::<PathBuf>::new();
    if !mounts.is_empty() {
        unsafe { cardinal_mount_paths((&mut paths as *mut Vec<PathBuf>).cast(), collect) };
    }
    let mut counts = HashMap::new();
    for mount in &mounts {
        *counts.entry(mount.id.clone()).or_insert(0) += 1;
    }
    for mount in &mut mounts {
        mount.nested = paths
            .iter()
            .filter(|p| *p != &mount.path && p.starts_with(&mount.path))
            .cloned()
            .collect();
        mount.nested.sort();
        if mount.id.is_empty() || counts[&mount.id] > 1 {
            // No stable identity, or two clones mounted at once. Never overwrite a shared cache.
            mount.id = format!("session-{}", mount.dev);
            mount.persistent = false;
        }
    }
    Some(mounts)
}

impl Mount {
    pub fn present(&self) -> bool {
        std::fs::metadata(&self.path).is_ok_and(|m| m.dev() == self.dev as u32 as u64)
    }

    pub fn verified_path(&self, path: &Path) -> bool {
        path.starts_with(&self.path)
            && !path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
            && discover().is_some_and(|v| {
                v.iter().any(|m| {
                    m.id == self.id
                        && m.dev == self.dev
                        && m.path == self.path
                        && !m.nested.iter().any(|root| path.starts_with(root))
                })
            })
    }
}

static WAKE: LazyLock<(Sender<()>, Receiver<()>)> = LazyLock::new(|| bounded(1));
extern "C" fn notify() {
    let _ = WAKE.0.try_send(());
}
pub(super) fn wake() {
    notify();
}
pub(super) fn subscribe() -> Receiver<()> {
    unsafe { cardinal_watch_mounts(notify) };
    WAKE.1.clone()
}
