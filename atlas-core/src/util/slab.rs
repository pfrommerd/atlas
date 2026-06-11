use std::collections::HashMap;

pub struct ShardedSlab<K, V> {
    map: HashMap<K, V>,
}

impl<K, V> Default for ShardedSlab<K, V> {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}
