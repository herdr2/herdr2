//! Bounded TTL in-memory result cache for zero-token deduplication of identical AI queries.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_MAX_ENTRIES: usize = 512;

struct Entry {
    value: String,
    expires_at: Instant,
    last_hit_at: Instant,
}

pub struct AiResultCache {
    max_entries: usize,
    entries: Mutex<HashMap<String, Entry>>,
}

impl Default for AiResultCache {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES)
    }
}

impl AiResultCache {
    pub fn new(max_entries: usize) -> Self {
        Self { max_entries: max_entries.max(16), entries: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let now = Instant::now();
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let entry = map.get_mut(key)?;
        if now >= entry.expires_at {
            map.remove(key);
            return None;
        }
        entry.last_hit_at = now;
        Some(entry.value.clone())
    }

    pub fn put(&self, key: String, value: String, ttl: Duration) {
        if ttl.is_zero() || key.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() >= self.max_entries {
            map.retain(|_, v| v.expires_at > now);
        }
        while map.len() >= self.max_entries {
            let oldest = map
                .iter()
                .min_by_key(|(_, v)| v.last_hit_at)
                .map(|(k, _)| k.clone());
            let Some(k) = oldest else { break };
            map.remove(&k);
        }
        map.insert(key, Entry { value, expires_at: now + ttl, last_hit_at: now });
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_and_evicts_lru_and_expired() {
        let c = AiResultCache::new(16);
        c.put("k1".into(), "v1".into(), Duration::from_secs(60));
        assert_eq!(c.get("k1").as_deref(), Some("v1"));

        c.put("k2".into(), "v2".into(), Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(c.get("k2"), None);
    }
}
