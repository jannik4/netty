use self::ptr::Ptr;
use std::{collections::HashMap, hash::Hash};

pub struct DoubleKeyMap<K1, K2, V> {
    data: Vec<(Ptr<K1>, Ptr<K2>, V)>,
    map1: HashMap<Ptr<K1>, usize>,
    map2: HashMap<Ptr<K2>, usize>,
}

impl<K1, K2, V> DoubleKeyMap<K1, K2, V> {
    pub fn new() -> Self {
        Self { data: Vec::new(), map1: HashMap::new(), map2: HashMap::new() }
    }

    pub fn insert(&mut self, k1: K1, k2: K2, v: V)
    where
        K1: Hash + Eq,
        K2: Hash + Eq,
    {
        // Remove the old values
        self.remove1(&k1);
        self.remove2(&k2);

        // Create ptrs to the keys
        let k1 = Ptr::new(k1);
        let k2 = Ptr::new(k2);

        // Insert into data
        let idx = self.data.len();
        self.data.push((k1, k2, v));

        // Insert into the maps
        self.map1.insert(k1, idx);
        self.map2.insert(k2, idx);
    }

    pub fn remove1(&mut self, key: &K1) -> Option<(K1, K2, V)>
    where
        K1: Hash + Eq,
        K2: Hash + Eq,
    {
        // Remove the element from the map
        let idx = self.map1.remove(key)?;

        // Swap the element to the end and remove it
        let (k1, k2, v) = self.data.swap_remove(idx);

        // Remove the element from the other map
        // This assert is essential to ensure that this fails loudly rather than causing
        // UB later on, if the keys Eq/Hash impls are broken.
        assert_eq!(self.map2.remove(&k2).unwrap(), idx);

        // Update the index in the maps of the element that was swapped in
        if idx < self.data.len() {
            let (k1, k2, _) = &self.data[idx];

            let map1_idx = self.map1.get_mut(k1).unwrap();
            assert_eq!(*map1_idx, self.data.len()); // Essential, see above
            *map1_idx = idx;

            let map2_idx = self.map2.get_mut(k2).unwrap();
            assert_eq!(*map1_idx, self.data.len()); // Essential, see above
            *map2_idx = idx;
        }

        // Safety: k1 and k2 are removed from data/map1/map2 and were never exposed
        // In more detail, each idx can at most be inserted once per map (see insert) and this
        // method removes exactly one idx from each map and asserts that the idx is the same.
        let (k1, k2) = unsafe { (*Ptr::into_owned(k1), *Ptr::into_owned(k2)) };

        Some((k1, k2, v))
    }

    pub fn remove2(&mut self, key: &K2) -> Option<(K1, K2, V)>
    where
        K1: Hash + Eq,
        K2: Hash + Eq,
    {
        // Remove the element from the map
        let idx = self.map2.remove(key)?;

        // Swap the element to the end and remove it
        let (k1, k2, v) = self.data.swap_remove(idx);

        // Remove the element from the other map
        // This assert is essential to ensure that this fails loudly rather than causing
        // UB later on, if the keys Eq/Hash impls are broken.
        assert_eq!(self.map1.remove(&k1).unwrap(), idx);

        // Update the index of the element that was swapped in
        if idx < self.data.len() {
            let (k1, k2, _) = &self.data[idx];

            let map1_idx = self.map1.get_mut(k1).unwrap();
            assert_eq!(*map1_idx, self.data.len()); // Essential, see above
            *map1_idx = idx;

            let map2_idx = self.map2.get_mut(k2).unwrap();
            assert_eq!(*map1_idx, self.data.len()); // Essential, see above
            *map2_idx = idx;
        }

        // Safety: k1 and k2 are removed from data/map1/map2 and were never exposed
        // In more detail, each idx can at most be inserted once per map (see insert) and this
        // method removes exactly one idx from each map and asserts that the idx is the same.
        let (k1, k2) = unsafe { (*Ptr::into_owned(k1), *Ptr::into_owned(k2)) };

        Some((k1, k2, v))
    }

    pub fn get1(&self, key: &K1) -> Option<(&K1, &K2, &V)>
    where
        K1: Hash + Eq,
    {
        let idx = *self.map1.get(key)?;
        let (k1, k2, v) = &self.data[idx];
        Some((&**k1, &**k2, v))
    }

    pub fn get2(&self, key: &K2) -> Option<(&K1, &K2, &V)>
    where
        K2: Hash + Eq,
    {
        let idx = *self.map2.get(key)?;
        let (k1, k2, v) = &self.data[idx];
        Some((&**k1, &**k2, v))
    }

    pub fn get1_mut(&mut self, key: &K1) -> Option<(&K1, &K2, &mut V)>
    where
        K1: Hash + Eq,
    {
        let idx = *self.map1.get(key)?;
        let (k1, k2, v) = &mut self.data[idx];
        Some((&**k1, &**k2, v))
    }

    pub fn get2_mut(&mut self, key: &K2) -> Option<(&K1, &K2, &mut V)>
    where
        K2: Hash + Eq,
    {
        let idx = *self.map2.get(key)?;
        let (k1, k2, v) = &mut self.data[idx];
        Some((&**k1, &**k2, v))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K1, &K2, &V)> {
        self.into_iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K1, &K2, &mut V)> {
        self.into_iter()
    }
}

impl<K1, K2, V> Default for DoubleKeyMap<K1, K2, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, K1, K2, V> IntoIterator for &'a DoubleKeyMap<K1, K2, V> {
    type Item = (&'a K1, &'a K2, &'a V);

    // TODO: type IntoIter = impl Iterator<Item = Self::Item>;
    type IntoIter = Box<dyn Iterator<Item = Self::Item> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.data.iter().map(|(k1, k2, v)| (&**k1, &**k2, v)))
    }
}

impl<'a, K1, K2, V> IntoIterator for &'a mut DoubleKeyMap<K1, K2, V> {
    type Item = (&'a K1, &'a K2, &'a mut V);

    // TODO: type IntoIter = impl Iterator<Item = Self::Item>;
    type IntoIter = Box<dyn Iterator<Item = Self::Item> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.data.iter_mut().map(|(k1, k2, v)| (&**k1, &**k2, v)))
    }
}

impl<K1, K2, V> IntoIterator for DoubleKeyMap<K1, K2, V>
// FIXME: Is the 'static bound really necessary?
where
    K1: 'static,
    K2: 'static,
    V: 'static,
{
    type Item = (K1, K2, V);

    // TODO: type IntoIter = impl Iterator<Item = Self::Item>;
    type IntoIter = Box<dyn Iterator<Item = Self::Item>>;

    fn into_iter(self) -> Self::IntoIter {
        drop(self.map1);
        drop(self.map2);

        Box::new(self.data.into_iter().map(|(k1, k2, v)| {
            // SAFETY: We just dropped the maps, so the keys are no longer used elsewhere.
            unsafe { (*Ptr::into_owned(k1), *Ptr::into_owned(k2), v) }
        }))
    }
}

mod ptr {
    use std::{
        borrow::Borrow,
        hash::{Hash, Hasher},
        ops::Deref,
    };

    /// Non-reference-counted read-only pointer.
    pub struct Ptr<T>(*const T);

    // Safety: Ptr only exposes read-only access to the value safely
    unsafe impl<T: Send> Send for Ptr<T> {}
    unsafe impl<T: Sync> Sync for Ptr<T> {}

    impl<T> Ptr<T> {
        pub fn new(value: T) -> Self {
            Self(Box::into_raw(Box::new(value)))
        }

        /// # Safety
        ///
        /// This must be the only ptr to the value and not used after this call.
        pub unsafe fn into_owned(this: Self) -> Box<T> {
            Box::from_raw(this.0 as *mut T)
        }

        // /// # Safety
        // ///
        // /// This must be the only ptr to the value and not used after this call.
        // pub unsafe fn drop(this: Self) {
        //     drop(Self::into_owned(this));
        // }
    }

    impl<T> Deref for Ptr<T> {
        type Target = T;

        fn deref(&self) -> &T {
            unsafe { &*self.0 }
        }
    }

    impl<T> Borrow<T> for Ptr<T> {
        fn borrow(&self) -> &T {
            self
        }
    }

    impl<T> Clone for Ptr<T> {
        fn clone(&self) -> Self {
            *self
        }
    }

    impl<T> Copy for Ptr<T> {}

    impl<T> PartialEq for Ptr<T>
    where
        T: PartialEq,
    {
        fn eq(&self, other: &Self) -> bool {
            **self == **other
        }
    }

    impl<T> Eq for Ptr<T> where T: Eq {}

    impl<T> Hash for Ptr<T>
    where
        T: Hash,
    {
        fn hash<H: Hasher>(&self, state: &mut H) {
            (**self).hash(state)
        }
    }
}
