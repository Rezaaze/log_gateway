use chrono::{DateTime, Utc};
use moka::sync::SegmentedCache;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub redacted_message: String,
    pub pii_hits: usize,
    pub created_at: DateTime<Utc>,
}

impl CacheEntry {
    pub fn inspect(&self) -> (&str, usize, DateTime<Utc>) {
        (&self.redacted_message, self.pii_hits, self.created_at)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheStats {
    pub total_requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_rate_percent: f64,
}

#[derive(Debug, Clone)]
pub struct SemanticCache {
    store: SegmentedCache<String, CacheEntry>,
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
}

impl SemanticCache {
    pub fn new(max_capacity: u64, ttl_seconds: u64) -> Self {
        let store = SegmentedCache::builder(32)
            .max_capacity(max_capacity)
            .time_to_live(Duration::from_secs(ttl_seconds))
            .build();

        Self {
            store,
            hits: Arc::new(AtomicU64::new(0)),
            misses: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn make_key(message: &str) -> String {
        let normalized = message.trim().to_lowercase();
        let mut hasher = Sha256::new();
        hasher.update(normalized.as_bytes());
        let result = hasher.finalize();
        hex::encode(result)
    }

    pub fn get(&self, key: &str) -> Option<CacheEntry> {
        match self.store.get(key) {
            Some(entry) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(entry)
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub fn insert(&self, key: String, entry: CacheEntry) {
        self.store.insert(key, entry);
    }

    pub fn stats(&self) -> CacheStats {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;

        let hit_rate_percent = if total == 0 {
            0.0
        } else {
            (hits as f64 / total as f64) * 100.0
        };

        CacheStats {
            total_requests: total,
            cache_hits: hits,
            cache_misses: misses,
            hit_rate_percent,
        }
    }

    pub fn len(&self) -> u64 {
        self.store.entry_count()
    }

    pub fn is_empty(&self) -> bool {
        self.store.entry_count() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_cache_miss_increments_counter() {
        let cache = SemanticCache::new(100, 60);
        let key = SemanticCache::make_key("test message");

        // Initial stats should be zero
        let stats = cache.stats();
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 0);

        // Get on empty cache should increment misses
        let result = cache.get(&key);
        assert!(result.is_none());

        let stats = cache.stats();
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 1);
    }

    #[test]
    fn test_cache_hit_increments_counter() {
        let cache = SemanticCache::new(100, 60);
        let key = SemanticCache::make_key("test message");
        let entry = CacheEntry {
            redacted_message: "redacted".to_string(),
            pii_hits: 0,
            created_at: Utc::now(),
        };

        // Insert then get
        cache.insert(key.clone(), entry);
        let result = cache.get(&key);
        assert!(result.is_some());

        let stats = cache.stats();
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.cache_misses, 0);
    }

    #[test]
    fn test_make_key_deterministic() {
        let key1 = SemanticCache::make_key("test message");
        let key2 = SemanticCache::make_key("test message");
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_make_key_case_insensitive() {
        let key1 = SemanticCache::make_key("Hello World");
        let key2 = SemanticCache::make_key("hello world");
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_stats_hit_rate_calculation() {
        let cache = SemanticCache::new(100, 60);
        let key = SemanticCache::make_key("test");
        let entry = CacheEntry {
            redacted_message: "redacted".to_string(),
            pii_hits: 0,
            created_at: Utc::now(),
        };

        // Insert and get 3 times (hits)
        cache.insert(key.clone(), entry.clone());
        cache.get(&key);
        cache.get(&key);
        cache.get(&key);

        // One miss
        let miss_key = SemanticCache::make_key("miss");
        cache.get(&miss_key);

        let stats = cache.stats();
        assert_eq!(stats.total_requests, 4);
        assert_eq!(stats.cache_hits, 3);
        assert_eq!(stats.cache_misses, 1);
        assert_eq!(stats.hit_rate_percent, 75.0);
    }

    #[test]
    fn test_stats_empty_cache() {
        let cache = SemanticCache::new(100, 60);
        let stats = cache.stats();
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 0);
        assert_eq!(stats.hit_rate_percent, 0.0);
    }
}
