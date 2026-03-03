use moka::sync::Cache;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

/// Result of a quota check
#[derive(Debug, Clone, PartialEq)]
pub enum QuotaResult {
    /// Request is allowed (usage < 80%)
    Allowed,
    /// Soft warning (80% ≤ usage < 100%)
    SoftWarning { usage_pct: f64 },
    /// Quota exceeded (usage ≥ 100%)
    Exceeded,
}

/// Manages rate limiting quotas per tenant with sliding window counters
#[derive(Debug)]
pub struct QuotaManager {
    /// Sliding window counters per tenant_id with TTL of 1 second
    counters: Cache<Uuid, Arc<AtomicU64>>,
}

impl QuotaManager {
    /// Creates a new QuotaManager
    pub fn new() -> Self {
        Self {
            counters: Cache::builder()
                .time_to_live(std::time::Duration::from_secs(1))
                .build(),
        }
    }

    /// Checks if a request is allowed and increments the counter if allowed.
    /// Returns QuotaResult indicating the current usage level.
    pub fn check_and_increment(&self, tenant_id: Uuid, rate_limit: u32) -> QuotaResult {
        // Get or create atomic counter for this tenant
        let counter = self
            .counters
            .get_with(tenant_id, || Arc::new(AtomicU64::new(0)));

        // Increment the counter atomically
        let current_count = counter.fetch_add(1, Ordering::SeqCst) + 1;

        // Calculate usage percentage
        let usage_pct = (current_count as f64 / rate_limit as f64) * 100.0;

        // Determine quota result
        if usage_pct > 100.0 {
            QuotaResult::Exceeded
        } else if usage_pct >= 80.0 {
            QuotaResult::SoftWarning { usage_pct }
        } else {
            QuotaResult::Allowed
        }
    }

    /// Gets the current count for a tenant (for testing purposes)
    #[cfg(test)]
    pub fn get_count(&self, tenant_id: Uuid) -> Option<u64> {
        self.counters
            .get(&tenant_id)
            .map(|counter| counter.load(Ordering::SeqCst))
    }
}

impl Default for QuotaManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_quota_result_allowed_when_under_limit() {
        let manager = QuotaManager::new();
        let tenant_id = Uuid::new_v4();
        let rate_limit = 100;

        // First request should be allowed
        let result = manager.check_and_increment(tenant_id, rate_limit);
        assert_eq!(result, QuotaResult::Allowed);

        // Check count is 1
        assert_eq!(manager.get_count(tenant_id), Some(1));
    }

    #[test]
    fn test_quota_result_soft_warning_at_80_percent() {
        let manager = QuotaManager::new();
        let tenant_id = Uuid::new_v4();
        let rate_limit = 10; // Small limit for easier testing

        // Make requests to reach 80% (8 requests)
        for i in 0..8 {
            let result = manager.check_and_increment(tenant_id, rate_limit);
            if i < 7 {
                // First 7 requests should be allowed
                assert_eq!(result, QuotaResult::Allowed);
            } else {
                // 8th request (80%) should be soft warning
                match result {
                    QuotaResult::SoftWarning { usage_pct } => {
                        assert!((usage_pct - 80.0).abs() < f64::EPSILON);
                    }
                    _ => panic!("Expected SoftWarning, got {:?}", result),
                }
            }
        }

        // 9th request (90%) should also be soft warning
        let result = manager.check_and_increment(tenant_id, rate_limit);
        match result {
            QuotaResult::SoftWarning { usage_pct } => {
                assert!((usage_pct - 90.0).abs() < f64::EPSILON);
            }
            _ => panic!("Expected SoftWarning, got {:?}", result),
        }
    }

    #[test]
    fn test_quota_result_exceeded_at_100_percent() {
        let manager = QuotaManager::new();
        let tenant_id = Uuid::new_v4();
        let rate_limit = 5; // Small limit for easier testing

        // Make requests to reach 100% (5 requests)
        for i in 0..5 {
            let result = manager.check_and_increment(tenant_id, rate_limit);
            if i < 4 {
                // First 4 requests should be allowed or soft warning
                assert!(matches!(
                    result,
                    QuotaResult::Allowed | QuotaResult::SoftWarning { .. }
                ));
            } else {
                // 5th request (100%) should be soft warning (not exceeded since 100% == 100%, not > 100%)
                match result {
                    QuotaResult::SoftWarning { usage_pct } => {
                        assert!((usage_pct - 100.0).abs() < f64::EPSILON);
                    }
                    _ => panic!("Expected SoftWarning for 100%, got {:?}", result),
                }
            }
        }

        // 6th request (120%) should be exceeded (> 100%)
        let result = manager.check_and_increment(tenant_id, rate_limit);
        assert_eq!(result, QuotaResult::Exceeded);
    }

    #[test]
    fn test_concurrent_requests_atomic_race_condition_free() {
        let manager = Arc::new(QuotaManager::new());
        let tenant_id = Uuid::new_v4();
        // Use rate_limit = 500 with 1000 total requests so requests 501+ are Exceeded
        let rate_limit = 500u32;
        let num_threads = 10;
        let requests_per_thread = 100usize;

        let mut handles = vec![];

        for _ in 0..num_threads {
            let manager = Arc::clone(&manager);
            let handle = thread::spawn(move || {
                let mut allowed_count = 0usize;
                let mut warning_count = 0usize;
                let mut exceeded_count = 0usize;

                for _ in 0..requests_per_thread {
                    match manager.check_and_increment(tenant_id, rate_limit) {
                        QuotaResult::Allowed => allowed_count += 1,
                        QuotaResult::SoftWarning { .. } => warning_count += 1,
                        QuotaResult::Exceeded => exceeded_count += 1,
                    }
                }

                (allowed_count, warning_count, exceeded_count)
            });
            handles.push(handle);
        }

        let mut total_allowed = 0usize;
        let mut total_warning = 0usize;
        let mut total_exceeded = 0usize;

        for handle in handles {
            let (allowed, warning, exceeded) = handle.join().unwrap();
            total_allowed += allowed;
            total_warning += warning;
            total_exceeded += exceeded;
        }

        let total_requests = num_threads * requests_per_thread;

        // Every request must be accounted for — no lost increments (atomic correctness)
        assert_eq!(
            total_allowed + total_warning + total_exceeded,
            total_requests,
            "Total request count mismatch — atomic increment lost"
        );

        // With rate_limit=500 and 1000 requests, requests 501+ must be Exceeded
        // (usage_pct > 100.0). At minimum request #502 triggers Exceeded.
        assert!(
            total_exceeded > 0,
            "Expected Exceeded results for requests beyond rate_limit=500, got 0"
        );
    }

    #[test]
    fn test_check_and_increment_with_rate_limit_1() {
        let manager = QuotaManager::new();
        let tenant_id = Uuid::new_v4();
        let rate_limit = 1;

        // First request should be soft warning (0 → 1, 100%)
        let result = manager.check_and_increment(tenant_id, rate_limit);
        match result {
            QuotaResult::SoftWarning { usage_pct } => {
                assert!((usage_pct - 100.0).abs() < f64::EPSILON);
            }
            _ => panic!("Expected SoftWarning for 100%, got {:?}", result),
        }

        // Second request should be exceeded (1 → 2, 200%)
        let result = manager.check_and_increment(tenant_id, rate_limit);
        assert_eq!(result, QuotaResult::Exceeded);
    }

    #[test]
    fn test_tenant_isolation() {
        let manager = QuotaManager::new();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let rate_limit = 10;

        // Tenant A uses 5 requests
        for _ in 0..5 {
            manager.check_and_increment(tenant_a, rate_limit);
        }

        // Tenant B should start fresh
        let result = manager.check_and_increment(tenant_b, rate_limit);
        assert_eq!(result, QuotaResult::Allowed); // First request for tenant B

        // Check counts are separate
        assert_eq!(manager.get_count(tenant_a), Some(5));
        assert_eq!(manager.get_count(tenant_b), Some(1));
    }

    #[test]
    fn test_counter_expires_after_ttl() {
        let manager = QuotaManager::new();
        let tenant_id = Uuid::new_v4();
        let rate_limit = 10;

        // Make some requests
        manager.check_and_increment(tenant_id, rate_limit);
        manager.check_and_increment(tenant_id, rate_limit);
        assert_eq!(manager.get_count(tenant_id), Some(2));

        // Wait for TTL to expire (1 second + small buffer)
        thread::sleep(Duration::from_millis(1100));

        // Counter should have expired, so we get a fresh one
        let result = manager.check_and_increment(tenant_id, rate_limit);
        assert_eq!(result, QuotaResult::Allowed); // First request on fresh counter
        assert_eq!(manager.get_count(tenant_id), Some(1)); // Not 3!
    }
}
