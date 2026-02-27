use dashmap::DashMap;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct TenantStats {
    pub tenant_id: String,
    pub total_requests: u64,
    pub total_bytes_ingested: u64,
    pub total_pii_hits: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatewayCostSummary {
    pub total_tenants: usize,
    pub total_requests: u64,
    pub total_bytes_ingested: u64,
    pub total_pii_hits: u64,
    pub tenants: Vec<TenantStats>,
}

#[derive(Debug, Clone)]
pub struct CostTracker {
    stats: Arc<DashMap<String, TenantStats>>,
}

impl CostTracker {
    pub fn new() -> Self {
        Self {
            stats: Arc::new(DashMap::new()),
        }
    }

    pub fn record(&self, tenant_id: &str, bytes: u64, pii_hits: usize, was_cache_hit: bool) {
        self.stats
            .entry(tenant_id.to_string())
            .and_modify(|stats| {
                stats.total_requests += 1;
                stats.total_bytes_ingested += bytes;
                stats.total_pii_hits += pii_hits as u64;
                if was_cache_hit {
                    stats.cache_hits += 1;
                } else {
                    stats.cache_misses += 1;
                }
            })
            .or_insert_with(|| TenantStats {
                tenant_id: tenant_id.to_string(),
                total_requests: 1,
                total_bytes_ingested: bytes,
                total_pii_hits: pii_hits as u64,
                cache_hits: if was_cache_hit { 1 } else { 0 },
                cache_misses: if was_cache_hit { 0 } else { 1 },
            });
    }

    pub fn summary(&self) -> GatewayCostSummary {
        let mut total_requests = 0;
        let mut total_bytes_ingested = 0;
        let mut total_pii_hits = 0;
        let mut tenants = Vec::new();

        for entry in self.stats.iter() {
            let stats = entry.value();
            total_requests += stats.total_requests;
            total_bytes_ingested += stats.total_bytes_ingested;
            total_pii_hits += stats.total_pii_hits;
            tenants.push(stats.clone());
        }

        // Sort tenants by total_requests descending
        tenants.sort_by(|a, b| b.total_requests.cmp(&a.total_requests));

        GatewayCostSummary {
            total_tenants: tenants.len(),
            total_requests,
            total_bytes_ingested,
            total_pii_hits,
            tenants,
        }
    }

    pub fn tenant_stats(&self, tenant_id: &str) -> Option<TenantStats> {
        self.stats.get(tenant_id).map(|entry| entry.value().clone())
    }
}

impl Default for CostTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_creates_tenant() {
        let tracker = CostTracker::new();
        tracker.record("tenant1", 100, 2, false);

        let stats = tracker.tenant_stats("tenant1");
        assert!(stats.is_some());
        let stats = stats.unwrap();
        assert_eq!(stats.tenant_id, "tenant1");
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.total_bytes_ingested, 100);
        assert_eq!(stats.total_pii_hits, 2);
    }

    #[test]
    fn test_record_increments_bytes() {
        let tracker = CostTracker::new();
        tracker.record("tenant1", 100, 0, false);
        tracker.record("tenant1", 100, 0, false);

        let stats = tracker.tenant_stats("tenant1").unwrap();
        assert_eq!(stats.total_bytes_ingested, 200);
        assert_eq!(stats.total_requests, 2);
    }

    #[test]
    fn test_cache_hit_miss_tracking() {
        let tracker = CostTracker::new();
        tracker.record("tenant1", 100, 0, true); // cache hit
        tracker.record("tenant1", 100, 0, false); // cache miss

        let stats = tracker.tenant_stats("tenant1").unwrap();
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.cache_misses, 1);
    }

    #[test]
    fn test_summary_sorts_by_requests_desc() {
        let tracker = CostTracker::new();
        tracker.record("tenant_a", 100, 0, false);
        tracker.record("tenant_a", 100, 0, false);
        tracker.record("tenant_a", 100, 0, false);
        tracker.record("tenant_a", 100, 0, false);
        tracker.record("tenant_a", 100, 0, false); // 5 requests

        tracker.record("tenant_b", 100, 0, false);
        tracker.record("tenant_b", 100, 0, false); // 2 requests

        let summary = tracker.summary();
        assert_eq!(summary.total_tenants, 2);
        assert_eq!(summary.total_requests, 7);

        // First tenant should be tenant_a (5 requests)
        assert_eq!(summary.tenants[0].tenant_id, "tenant_a");
        assert_eq!(summary.tenants[0].total_requests, 5);

        // Second tenant should be tenant_b (2 requests)
        assert_eq!(summary.tenants[1].tenant_id, "tenant_b");
        assert_eq!(summary.tenants[1].total_requests, 2);
    }

    #[test]
    fn test_tenant_stats_not_found() {
        let tracker = CostTracker::new();
        let stats = tracker.tenant_stats("nonexistent");
        assert!(stats.is_none());
    }

    #[test]
    fn test_pii_hits_accumulate() {
        let tracker = CostTracker::new();
        tracker.record("tenant1", 100, 2, false);
        tracker.record("tenant1", 100, 2, false);
        tracker.record("tenant1", 100, 2, false);

        let stats = tracker.tenant_stats("tenant1").unwrap();
        assert_eq!(stats.total_pii_hits, 6);
    }
}
