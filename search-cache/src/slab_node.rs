use crate::{NAME_POOL, OptionSlabIndex, SlabIndex, State, StateTypeSize};
use fswalk::NodeFileType;
use serde::{
    Deserialize, Serialize,
    de::{self, SeqAccess, Visitor},
    ser::SerializeTuple,
};
use std::{fmt, num::NonZeroU32};
use thin_vec::ThinVec;

#[derive(Debug, Clone, Copy)]
pub struct NameAndParent {
    ptr: *const u8,
    // Length of the filename should not be larger than 256 chars(macOS, Linux,
    // Window, BSD) should be enough.
    len: u32,
    parent: OptionSlabIndex,
}

unsafe impl Send for NameAndParent {}
unsafe impl Sync for NameAndParent {}

impl Serialize for NameAndParent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut s = serializer.serialize_tuple(2)?;
        s.serialize_element(self.as_str())?;
        s.serialize_element(&self.parent)?;
        s.end()
    }
}

impl<'de> serde::de::Deserialize<'de> for NameAndParent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        struct NameAndParentVisitor;

        impl<'de> Visitor<'de> for NameAndParentVisitor {
            type Value = NameAndParent;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a tuple of (string, OptionSlabIndex)")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let name: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let parent: OptionSlabIndex = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;

                let name_in_pool = NAME_POOL.push(&name);

                Ok(NameAndParent::new(name_in_pool, parent))
            }
        }

        deserializer.deserialize_tuple(2, NameAndParentVisitor)
    }
}

impl std::ops::Deref for NameAndParent {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl NameAndParent {
    pub fn new(s: &'static str, parent: OptionSlabIndex) -> Self {
        Self {
            ptr: s.as_ptr(),
            len: s
                .len()
                .try_into()
                .expect("get filename larger than 256 bytes"),
            parent,
        }
    }

    pub fn as_str(&self) -> &'static str {
        unsafe { std::str::from_raw_parts(self.ptr, self.len as usize) }
    }

    pub fn parent(&self) -> Option<SlabIndex> {
        self.parent.to_option()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SlabNode<M = SlabNodeMetadataCompact> {
    name_and_parent: NameAndParent,
    pub children: ThinVec<SlabIndex>,
    pub metadata: M,
}

impl SlabNode {
    pub fn name(&self) -> &'static str {
        self.name_and_parent.as_str()
    }

    pub fn parent(&self) -> Option<SlabIndex> {
        self.name_and_parent.parent()
    }

    pub fn file_type_hint(&self) -> NodeFileType {
        self.metadata.file_type_hint()
    }

    pub fn state(&self) -> State {
        self.metadata.state()
    }

    pub fn add_children(&mut self, children: SlabIndex) {
        if !self.children.contains(&children) {
            self.children.push(children);
        }
    }

    pub fn new(
        parent: Option<SlabIndex>,
        name: &'static str,
        metadata: SlabNodeMetadataCompact,
    ) -> Self {
        Self {
            name_and_parent: NameAndParent::new(name, OptionSlabIndex::from_option(parent)),
            children: ThinVec::new(),
            metadata,
        }
    }
}

/// SlabNodeMetadataCompact with state ensured to be Some
pub struct SlabNodeMetadata<'a>(&'a SlabNodeMetadataCompact);

impl<'a> SlabNodeMetadata<'a> {
    pub fn r#type(&self) -> fswalk::NodeFileType {
        self.0.state_type_and_size.r#type()
    }

    pub fn size(&self) -> i64 {
        self.0.state_type_and_size.size()
    }

    pub fn allocated_size(&self) -> Option<i64> {
        (self.r#type() != NodeFileType::Dir && self.0.allocated_size >= 0)
            .then_some(self.0.allocated_size)
    }

    pub fn ctime(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.0.ctime)
    }

    pub fn mtime(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.0.mtime)
    }
}

// The old wire format ended after the timestamps. Decode directly into the new slab
// without re-enumerating the filesystem or allocating a second copy of the tree.
#[derive(Deserialize)]
pub(crate) struct LegacyNodeMetadata {
    state_type_and_size: StateTypeSize,
    ctime: u32,
    mtime: u32,
}

impl From<SlabNode<LegacyNodeMetadata>> for SlabNode {
    fn from(node: SlabNode<LegacyNodeMetadata>) -> Self {
        Self {
            name_and_parent: node.name_and_parent,
            children: node.children,
            metadata: SlabNodeMetadataCompact {
                state_type_and_size: node.metadata.state_type_and_size,
                ctime: node.metadata.ctime,
                mtime: node.metadata.mtime,
                allocated_size: ALLOCATION_PENDING,
            },
        }
    }
}

const ALLOCATION_PENDING: i64 = -1;
const ALLOCATION_UNREADABLE: i64 = -2;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct SlabNodeMetadataCompact {
    state_type_and_size: StateTypeSize,
    // Actually a Option<NonZeroU32>, but using u32 here due to https://github.com/serde-rs/serde/issues/1834
    ctime: u32,
    mtime: u32,
    allocated_size: i64,
}

impl SlabNodeMetadataCompact {
    pub fn unaccessible() -> Self {
        Self {
            state_type_and_size: StateTypeSize::unaccessible(),
            allocated_size: ALLOCATION_UNREADABLE,
            ctime: 0,
            mtime: 0,
        }
    }

    pub fn some(
        fswalk::NodeMetadata {
            r#type,
            size,
            allocated_size,
            ctime,
            mtime,
        }: fswalk::NodeMetadata,
    ) -> Self {
        Self {
            state_type_and_size: StateTypeSize::some(r#type, size),
            allocated_size: allocated_size.min(i64::MAX as u64) as i64,
            ctime: ctime
                .and_then(|x| std::num::NonZeroU32::try_from(x).ok())
                .map(|x| x.get())
                .unwrap_or_default(),
            mtime: mtime
                .and_then(|x| std::num::NonZeroU32::try_from(x).ok())
                .map(|x| x.get())
                .unwrap_or_default(),
        }
    }

    pub fn none() -> Self {
        Self {
            state_type_and_size: StateTypeSize::none(),
            allocated_size: ALLOCATION_PENDING,
            ctime: 0,
            mtime: 0,
        }
    }

    pub fn state(&self) -> State {
        self.state_type_and_size.state()
    }

    pub fn as_ref(&self) -> Option<SlabNodeMetadata<'_>> {
        match self.state() {
            State::Some => Some(SlabNodeMetadata(self)),
            State::Unaccessible | State::None => None,
        }
    }

    pub fn is_some(&self) -> bool {
        matches!(self.state(), State::Some)
    }

    pub fn is_none(&self) -> bool {
        matches!(self.state(), State::None)
    }

    pub fn is_unaccessible(&self) -> bool {
        matches!(self.state(), State::Unaccessible)
    }

    pub(crate) fn needs_hydration(&self, disk_usage: bool) -> bool {
        self.is_none()
            || (disk_usage
                && self.is_some()
                && self.file_type_hint() != NodeFileType::Dir
                && self.allocated_size == ALLOCATION_PENDING)
    }

    pub(crate) fn hydrate_path(&mut self, path: &std::path::Path) {
        let metadata = std::fs::symlink_metadata(path)
            .map(|data| Self::some(data.into()))
            .unwrap_or_else(|_| Self::unaccessible());
        self.apply_hydration(metadata);
    }

    /// A failed allocation lookup must preserve the dates and logical size of a legacy row.
    pub(crate) fn apply_hydration(&mut self, metadata: Self) {
        if self.is_some() && metadata.is_unaccessible() {
            self.allocated_size = ALLOCATION_UNREADABLE;
        } else {
            *self = metadata;
        }
    }

    pub fn file_type_hint(&self) -> NodeFileType {
        self.state_type_and_size.r#type()
    }
}

#[derive(Debug, Clone)]
pub struct SearchResultNode {
    pub path: std::path::PathBuf,
    pub metadata: SlabNodeMetadataCompact,
}
