use core::hash::Hash;
use std::collections::HashMap;

pub struct IdMap<K, V> {
    pub map:  HashMap<K, V>,
    next_key: u64,
}

impl<K, V> IdMap<K, V>
where K: Clone + Eq + From<u64> + Hash,
{
    pub fn add(&mut self, value: V) -> K {
        let key = self.next_id();
        self.map.insert(key.clone(), value);
        key
    }

    pub fn next_id(&mut self) -> K {
        let key = K::from(self.next_key);
        self.next_key += 1;
        key
    }
}

impl<K, V> Default for IdMap<K, V> {
    fn default() -> Self {
        Self {
            map:      Default::default(),
            next_key: Default::default(),
        }
    }
}
