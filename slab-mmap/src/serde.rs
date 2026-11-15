use super::{Slab, builder::Builder};
use crate::INITIAL_SLOTS;
use core::{fmt, marker::PhantomData};
use serde::{
    de::{Deserialize, Deserializer, Error as DeError, MapAccess, Visitor},
    ser::{Serialize, SerializeMap, Serializer},
};
use std::num::NonZeroUsize;

impl<T> Serialize for Slab<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map_serializer = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in self {
            map_serializer.serialize_key(&key)?;
            map_serializer.serialize_value(value)?;
        }
        map_serializer.end()
    }
}

struct SlabVisitor<T, Wire = T>(PhantomData<(T, Wire)>);

impl<'de, T, Wire> Visitor<'de> for SlabVisitor<T, Wire>
where
    Wire: Deserialize<'de> + Into<T>,
{
    type Value = Slab<T>;

    fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "a map")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let size = map.size_hint().unwrap_or_default();
        let size = NonZeroUsize::new(size).unwrap_or(INITIAL_SLOTS);
        let mut builder = Builder::with_capacity(size).map_err(A::Error::custom)?;

        while let Some((key, value)) = map.next_entry::<usize, Wire>()? {
            builder.pair(key, value.into()).map_err(A::Error::custom)?
        }

        Ok(builder.build())
    }
}

impl<T> Slab<T> {
    /// Upgrade each stored value directly into its original slot, including sparse slabs.
    pub fn deserialize_from<'de, Wire, D>(deserializer: D) -> Result<Self, D::Error>
    where
        Wire: Deserialize<'de> + Into<T>,
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(SlabVisitor::<T, Wire>(PhantomData))
    }
}

impl<'de, T> Deserialize<'de> for Slab<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::deserialize_from::<T, _>(deserializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::{
        Visitor,
        value::{Error as ValueError, MapDeserializer},
    };

    struct ExpectingDisplay<'a>(&'a SlabVisitor<u32>);

    impl fmt::Display for ExpectingDisplay<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            Visitor::expecting(self.0, f)
        }
    }

    #[test]
    fn visitor_reports_map_expectation() {
        let visitor = SlabVisitor::<u32>(PhantomData);
        let rendered = format!("{}", ExpectingDisplay(&visitor));
        assert_eq!(rendered, "a map");
    }

    #[test]
    fn visitor_reconstructs_slab_from_map_entries() {
        let visitor = SlabVisitor::<u32>(PhantomData);
        let entries = vec![(0usize, 10u32), (3usize, 30u32), (7usize, 70u32)];
        let map = MapDeserializer::<_, ValueError>::new(entries.clone().into_iter());

        let slab = visitor
            .visit_map(map)
            .expect("visit_map should rebuild slab from entries");

        assert_eq!(slab.len(), entries.len());
        for (index, value) in entries {
            assert_eq!(slab.get(index), Some(&value));
        }
    }
}
