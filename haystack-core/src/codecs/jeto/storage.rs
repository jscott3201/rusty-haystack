//! Explicit request-owned capacities; no allocator instrumentation or claims
//! about allocator metadata/RSS. Freed growth buffers remain charged.
use super::*;

pub(super) fn reserve<T, M: Meter>(
    values: &mut Vec<T>,
    needed: usize,
    meter: &mut M,
) -> Result<(), Error<M::Error>> {
    if needed > values.capacity() {
        let capacity = needed
            .checked_next_power_of_two()
            .ok_or(Error::Allocation)?;
        let bytes = capacity
            .checked_mul(std::mem::size_of::<T>())
            .ok_or(Error::Allocation)?;
        charge(meter, Charge::Retained(bytes))?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| Error::Allocation)?;
    }
    Ok(())
}

/// A sorted request-local index. Build by appending, then sort once; inserting
/// into an ordered Vec one key at a time would impose quadratic shifts.
#[derive(Debug)]
pub(super) struct Index<K, V>(Vec<(K, V)>);
impl<K: AsRef<str>, V> Index<K, V> {
    pub(super) fn new() -> Self {
        Self(Vec::new())
    }
    pub(super) fn with_capacity<M: Meter>(
        count: usize,
        meter: &mut M,
    ) -> Result<Self, Error<M::Error>> {
        let mut result = Self::new();
        reserve(&mut result.0, count, meter)?;
        Ok(result)
    }
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }
    pub(super) fn get(&self, name: &str) -> Option<&V> {
        self.0
            .binary_search_by(|(key, _)| key.as_ref().cmp(name))
            .ok()
            .map(|at| &self.0[at].1)
    }
    pub(super) fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = &K> {
        self.0.iter().map(|(key, _)| key)
    }
    pub(super) fn push<M: Meter>(
        &mut self,
        key: K,
        value: V,
        meter: &mut M,
    ) -> Result<(), Error<M::Error>> {
        let needed = self.len().checked_add(1).ok_or(Error::Allocation)?;
        reserve(&mut self.0, needed, meter)?;
        self.0.push((key, value));
        Ok(())
    }
    pub(super) fn finish<M: Meter>(&mut self, meter: &mut M) -> Result<bool, Error<M::Error>> {
        for (key, _) in &self.0 {
            tree_charge(key.as_ref(), self.len(), meter)?;
        }
        self.0
            .sort_unstable_by(|(a, _), (b, _)| a.as_ref().cmp(b.as_ref()));
        Ok(self
            .0
            .windows(2)
            .all(|pair| pair[0].0.as_ref() != pair[1].0.as_ref()))
    }
}
impl<'a, K, V> IntoIterator for &'a Index<K, V> {
    type Item = &'a (K, V);
    type IntoIter = std::slice::Iter<'a, (K, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
pub(super) type Columns<'a> = Index<&'a str, Option<&'a str>>;

pub(super) fn dict<M: Meter>(
    count: usize,
    meter: &mut M,
) -> Result<crate::data::HDict, Error<M::Error>> {
    if count != 0 {
        // Rust 1.99 HashMap/hashbrown uses power-of-two buckets, at most 7/8
        // occupancy and a minimum 4-bucket allocation. Twice the entry count,
        // rounded up, bounds buckets; include control bytes and group padding.
        // This charges table payload, not a measured total heap footprint.
        let buckets = count
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(Error::Allocation)?
            .max(4);
        let bytes = buckets
            .checked_mul(std::mem::size_of::<(String, Kind)>() + 1)
            .and_then(|n| n.checked_add(32))
            .ok_or(Error::Allocation)?;
        charge(meter, Charge::Retained(bytes))?;
    }
    charge(
        meter,
        Charge::Retained(std::mem::size_of::<crate::data::HDict>()),
    )?;
    crate::data::HDict::try_with_capacity(count).map_err(|_| Error::Allocation)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_capacity_reservation_leaves_storage_untouched() {
        let mut values = Vec::<[u8; 97]>::new();
        let mut rejecting = |_: Charge| Err("stop");
        assert!(matches!(
            reserve(&mut values, 1, &mut rejecting),
            Err(Error::Budget("stop"))
        ));
        assert_eq!(values.capacity(), 0);
        let mut charged = 0;
        let mut accepting = |cost| {
            if let Charge::Retained(bytes) = cost {
                charged += bytes;
            }
            Ok::<_, ()>(())
        };
        reserve(&mut values, 1, &mut accepting).unwrap();
        values.push([0; 97]);
        reserve(&mut values, 2, &mut accepting).unwrap();
        assert_eq!(values.capacity(), 2);
        assert_eq!(charged, 3 * 97);
        let capacity = values.capacity();
        assert!(reserve(&mut values, 3, &mut rejecting).is_err());
        assert_eq!(values.capacity(), capacity);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    #[test]
    fn borrowed_columns_and_native_tables_reserve_initial_storage() {
        let column_bytes = std::mem::size_of::<(&str, Option<&str>)>();
        let mut tight = budget::Bounded::new(Limits {
            max_retained_bytes: column_bytes - 1,
            ..Limits::default()
        })
        .unwrap();
        assert!(matches!(
            Columns::with_capacity(1, &mut tight),
            Err(Error::Budget(Limit::Retained))
        ));
        let mut sufficient = budget::Bounded::new(Limits {
            max_retained_bytes: column_bytes,
            ..Limits::default()
        })
        .unwrap();
        let mut columns = Columns::with_capacity(1, &mut sufficient).unwrap();
        columns
            .push("n", Some("sys::Number"), &mut sufficient)
            .unwrap();
        assert!(columns.finish(&mut sufficient).unwrap());
        assert_eq!(columns.get("n"), Some(&Some("sys::Number")));

        // A fresh one-member native Dict needs four hash buckets, even though
        // only one slot is live. Include the control/padding bound and handle.
        let table_bytes = 4 * (std::mem::size_of::<(String, Kind)>() + 1) + 32;
        let total = table_bytes + std::mem::size_of::<crate::data::HDict>();
        let mut tight = budget::Bounded::new(Limits {
            max_retained_bytes: table_bytes - 1,
            ..Limits::default()
        })
        .unwrap();
        assert!(matches!(
            dict(1, &mut tight),
            Err(Error::Budget(Limit::Retained))
        ));
        let mut sufficient = budget::Bounded::new(Limits {
            max_retained_bytes: total,
            ..Limits::default()
        })
        .unwrap();
        let mut native = dict(1, &mut sufficient).unwrap();
        let capacity = native.tags().capacity();
        assert_eq!(capacity, 3);
        native.set("x", Kind::Bool(true));
        assert_eq!(native.tags().capacity(), capacity);
    }
}
