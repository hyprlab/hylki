/**
 * @file bounded_cache.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Bounded capacity TTL cache with negative hit caching.
 */

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
enum CacheEntry<V> {
    Hit { value: V, expires: Instant },
    Negative { expires: Instant },
}

pub struct BoundedCache<K, V> {
    entries: Mutex<HashMap<K, CacheEntry<V>>>,
    lru_keys: Mutex<Vec<K>>,
    capacity: usize,
    ttl: Duration,
    negative_ttl: Duration,
}

impl<K: Hash + Eq + Clone, V: Clone> BoundedCache<K, V> {
    pub fn new(capacity: usize, ttl: Duration, negative_ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::with_capacity(capacity)),
            lru_keys: Mutex::new(Vec::with_capacity(capacity)),
            capacity,
            ttl,
            negative_ttl,
        }
    }

    pub fn get(&self, key: &K) -> Option<Option<V>> {
        let mut entries = self.entries.lock().unwrap();
        let now = Instant::now();

        if let Some(entry) = entries.get(key) {
            match entry {
                CacheEntry::Hit { value, expires } => {
                    if *expires > now {
                        let mut lru = self.lru_keys.lock().unwrap();
                        if let Some(pos) = lru.iter().position(|k| k == key) {
                            lru.remove(pos);
                        }
                        lru.push(key.clone());
                        return Some(Some(value.clone()));
                    }
                }
                CacheEntry::Negative { expires } => {
                    if *expires > now {
                        return Some(None);
                    }
                }
            }
            entries.remove(key);
        }
        None
    }

    pub fn insert(&self, key: K, value: V) {
        let mut entries = self.entries.lock().unwrap();
        let mut lru = self.lru_keys.lock().unwrap();
        let now = Instant::now();

        if entries.len() >= self.capacity {
            while let Some(oldest) = lru.first().cloned() {
                lru.remove(0);
                entries.remove(&oldest);
                if entries.len() < self.capacity {
                    break;
                }
            }
        }

        let expires = now + self.ttl;
        entries.insert(key.clone(), CacheEntry::Hit { value, expires });
        if let Some(pos) = lru.iter().position(|k| *k == key) {
            lru.remove(pos);
        }
        lru.push(key);
    }

    pub fn insert_negative(&self, key: K) {
        let mut entries = self.entries.lock().unwrap();
        let now = Instant::now();
        let expires = now + self.negative_ttl;
        entries.insert(key, CacheEntry::Negative { expires });
    }

    pub fn remove(&self, key: &K) {
        let mut entries = self.entries.lock().unwrap();
        let mut lru = self.lru_keys.lock().unwrap();
        entries.remove(key);
        if let Some(pos) = lru.iter().position(|k| k == key) {
            lru.remove(pos);
        }
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
        self.lru_keys.lock().unwrap().clear();
    }
}
