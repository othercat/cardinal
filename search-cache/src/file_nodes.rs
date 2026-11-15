use crate::{SlabIndex, SlabNode, ThinSlab};
use std::{
    ffi::OsStr,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct FileNodes {
    path: PathBuf,
    ignore_paths: Vec<PathBuf>,
    include_paths: Vec<PathBuf>,
    slab: ThinSlab<SlabNode>,
    root: SlabIndex,
    volume_rooted: bool,
}

impl FileNodes {
    pub(crate) fn new(
        path: PathBuf,
        ignore_paths: Vec<PathBuf>,
        include_paths: Vec<PathBuf>,
        slab: ThinSlab<SlabNode>,
        root: SlabIndex,
    ) -> Self {
        Self {
            path,
            ignore_paths,
            include_paths,
            slab,
            root,
            volume_rooted: false,
        }
    }

    pub(crate) fn root(&self) -> SlabIndex {
        self.root
    }

    // Compare path components at the first divergence, without constructing
    // two full paths on every probe of a same-name bucket.
    pub(crate) fn compare_paths(&self, mut a: SlabIndex, mut b: SlabIndex) -> std::cmp::Ordering {
        if a == b {
            return std::cmp::Ordering::Equal;
        }
        if self[a].parent() == self[b].parent() {
            return self[a].name().cmp(self[b].name());
        }
        let depth = |id| std::iter::successors(Some(id), |&i| self[i].parent()).count();
        let (da, db) = (depth(a), depth(b));
        for _ in db..da {
            a = self[a].parent().unwrap();
        }
        for _ in da..db {
            b = self[b].parent().unwrap();
        }
        if a == b {
            return da.cmp(&db);
        }
        while self[a].parent() != self[b].parent() {
            a = self[a].parent().unwrap();
            b = self[b].parent().unwrap();
        }
        self[a].name().cmp(self[b].name())
    }

    pub fn node_path(&self, index: SlabIndex) -> Option<PathBuf> {
        let mut current = index;
        let mut segments = vec![];
        while let Some(parent) = self.slab.get(current)?.parent() {
            segments.push(self.slab.get(current)?.name());
            current = parent;
        }
        let mut path = self.tree_root().to_owned();
        path.extend(segments.into_iter().rev().map(OsStr::new));
        Some(path)
    }

    pub(crate) fn tree_root(&self) -> &Path {
        if self.volume_rooted {
            &self.path
        } else {
            Path::new("/")
        }
    }

    pub(crate) fn use_volume_root(&mut self, mount: &Path) {
        self.volume_rooted = true;
        self.path = mount.to_owned();
    }

    pub(crate) fn index_only(&self) -> bool {
        self.volume_rooted
    }

    pub(crate) fn set_paths(&mut self, ignore: &[PathBuf], include: &[PathBuf]) {
        self.ignore_paths = ignore.to_vec();
        self.include_paths = include.to_vec();
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn ignore_paths(&self) -> &Vec<PathBuf> {
        &self.ignore_paths
    }

    pub(crate) fn include_paths(&self) -> &Vec<PathBuf> {
        &self.include_paths
    }

    pub(crate) fn take_slab(&mut self) -> ThinSlab<SlabNode> {
        std::mem::take(&mut self.slab)
    }

    pub(crate) fn put_slab(&mut self, slab: ThinSlab<SlabNode>) {
        self.slab = slab;
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        PathBuf,
        Vec<PathBuf>,
        Vec<PathBuf>,
        SlabIndex,
        ThinSlab<SlabNode>,
    ) {
        let Self {
            path,
            ignore_paths,
            include_paths,
            slab,
            root,
            volume_rooted: _,
        } = self;
        (path, ignore_paths, include_paths, root, slab)
    }
}

impl Deref for FileNodes {
    type Target = ThinSlab<SlabNode>;

    fn deref(&self) -> &Self::Target {
        &self.slab
    }
}

impl DerefMut for FileNodes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.slab
    }
}
