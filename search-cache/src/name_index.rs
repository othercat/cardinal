use crate::{FileNodes, NAME_POOL, SlabIndex};
use itertools::Itertools;
use search_cancel::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Instant};
use thin_vec::ThinVec;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[repr(transparent)]
#[serde(transparent)]
pub struct SortedSlabIndices {
    indices: ThinVec<SlabIndex>,
}

impl SortedSlabIndices {
    pub fn new(index: SlabIndex) -> Self {
        Self {
            indices: ThinVec::from_iter([index]),
        }
    }

    pub fn len(&self) -> usize {
        self.indices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &SlabIndex> {
        self.indices.iter()
    }

    pub(crate) fn as_slice(&self) -> &[SlabIndex] {
        &self.indices
    }

    pub fn insert(&mut self, index: SlabIndex, slab: &FileNodes) {
        if slab.get(index).is_none() {
            return;
        }
        if let Err(pos) = self
            .indices
            .binary_search_by(|existing| slab.compare_paths(*existing, index))
        {
            self.indices.insert(pos, index);
        }
    }

    /// # Safety
    ///
    /// The index must be inserted with it's full path ordered.
    pub unsafe fn insert_ordered(&mut self, index: SlabIndex) {
        self.indices.push(index);
    }

    pub fn remove(&mut self, index: SlabIndex) -> bool {
        if let Some(pos) = self.indices.iter().position(|&existing| existing == index) {
            self.indices.remove(pos);
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Default)]
pub struct NameIndex {
    map: BTreeMap<&'static str, SortedSlabIndices>,
}

impl NameIndex {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn all_indices(&self, cancellation_token: CancellationToken) -> Option<Vec<SlabIndex>> {
        self.map
            .values()
            .flat_map(|indices| indices.iter().copied())
            .enumerate()
            .map(|(i, index)| {
                cancellation_token
                    .is_cancelled_sparse(i)
                    .map(|()| index)
                    .ok_or(())
            })
            .try_collect()
            .ok()
    }

    pub fn get(&self, name: &str) -> Option<&SortedSlabIndices> {
        self.map.get(name)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&&'static str, &SortedSlabIndices)> {
        self.map.iter()
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut SortedSlabIndices> {
        self.map.get_mut(name)
    }

    /// # Safety
    ///
    /// The index must be inserted with it's full path ordered.
    pub unsafe fn add_index_ordered(&mut self, name: &str, index: SlabIndex) {
        unsafe {
            self.add_interned_ordered(NAME_POOL.push(name), index);
        }
    }

    pub(crate) unsafe fn add_interned_ordered(&mut self, name: &'static str, index: SlabIndex) {
        self.map
            .entry(name)
            .and_modify(|bucket| unsafe { bucket.insert_ordered(index) })
            .or_insert_with(|| SortedSlabIndices::new(index));
    }

    pub fn add_index(&mut self, name: &str, index: SlabIndex, slab: &FileNodes) {
        self.add_interned(NAME_POOL.push(name), index, slab);
    }

    pub(crate) fn add_interned(&mut self, name: &'static str, index: SlabIndex, slab: &FileNodes) {
        self.map
            .entry(name)
            .and_modify(|bucket| bucket.insert(index, slab))
            .or_insert_with(|| SortedSlabIndices::new(index));
    }

    pub fn remove_index(&mut self, name: &str, index: SlabIndex) -> bool {
        let Some(indices) = self.map.get_mut(name) else {
            return false;
        };
        let removed = indices.remove(index);
        if indices.is_empty() {
            self.map.remove(name);
        }
        removed
    }

    pub fn remove(&mut self, name: &str) -> Option<SortedSlabIndices> {
        self.map.remove(name)
    }

    pub(crate) fn as_persistent(&self) -> BTreeMap<Box<str>, SortedSlabIndices> {
        self.map
            .iter()
            .map(|(name, indices)| ((*name).to_string().into_boxed_str(), indices.clone()))
            .collect()
    }

    pub fn into_persistent(self) -> BTreeMap<Box<str>, SortedSlabIndices> {
        self.map
            .into_iter()
            .map(|(name, indices)| (name.to_string().into_boxed_str(), indices))
            .collect()
    }

    pub fn construct_name_pool(data: BTreeMap<Box<str>, SortedSlabIndices>) -> Self {
        let name_pool_time = Instant::now();
        let mut map = BTreeMap::new();
        for (name, indices) in data {
            let interned = NAME_POOL.push(&name);
            map.insert(interned, indices);
        }
        info!(
            "Name pool construction time: {:?}, count: {}",
            name_pool_time.elapsed(),
            NAME_POOL.len(),
        );
        Self { map }
    }
}
