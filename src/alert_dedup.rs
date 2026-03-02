use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

/// Berechnet einen deterministischen Fingerprint für ein Anomalie-Ereignis.
/// Gleicher Prefix + gleicher Origin-AS + gleicher Anomalie-Typ → gleicher Fingerprint.
/// Verwendung: SHA-256 von "{alert_type}:{prefix}:{origin_as}"
///
/// # Arguments
///
/// * `alert_type` - Der Typ der Anomalie (z.B. "hijack", "flap")
/// * `prefix` - Das BGP-Präfix
/// * `origin_as` - Die Origin-AS-Nummer
///
/// # Returns
///
/// Hex-String (64 Zeichen) des SHA-256-Hashes
pub fn compute_fingerprint(alert_type: &str, prefix: &str, origin_as: u32) -> String {
    let input = format!("{}:{}:{}", alert_type, prefix, origin_as);
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

/// Cache für Alert-Deduplikation mit TTL von 30 Minuten.
///
/// Gleiche Alerts innerhalb von 30 Minuten werden unterdrückt.
pub struct DedupCache {
    inner: moka::sync::Cache<String, DateTime<Utc>>,
}

impl DedupCache {
    /// Erstellt einen neuen DedupCache mit TTL von 30 Minuten.
    pub fn new() -> Self {
        use std::time::Duration;

        let cache = moka::sync::Cache::builder()
            .max_capacity(100_000) // max 100k eindeutige Fingerprints
            .time_to_live(Duration::from_secs(30 * 60)) // 30 Minuten
            .build();

        Self { inner: cache }
    }

    /// Prüft, ob ein Alert NEU ist (noch nicht im Cache).
    ///
    /// Gibt `true` zurück, wenn der Alert neu ist und wurde in den Cache eingetragen.
    /// Gibt `false` zurück, wenn der Alert ein Duplikat ist (bereits im Cache).
    ///
    /// # Arguments
    ///
    /// * `fingerprint` - Der Fingerprint des Alerts
    pub fn is_new(&self, fingerprint: &str) -> bool {
        if self.inner.contains_key(fingerprint) {
            false
        } else {
            self.inner.insert(fingerprint.to_string(), Utc::now());
            true
        }
    }

    /// Entfernt einen Fingerprint manuell aus dem Cache (z.B. nach Resolve).
    ///
    /// # Arguments
    ///
    /// * `fingerprint` - Der Fingerprint des Alerts
    pub fn invalidate(&self, fingerprint: &str) {
        self.inner.invalidate(fingerprint);
    }
}

impl Default for DedupCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_deterministic() {
        // Gleiche Inputs → gleicher Hash
        let fp1 = compute_fingerprint("hijack", "192.0.2.0/24", 64512);
        let fp2 = compute_fingerprint("hijack", "192.0.2.0/24", 64512);
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64); // SHA-256 hex string length
    }

    #[test]
    fn test_fingerprint_different_inputs() {
        // Unterschiedliche Inputs → unterschiedliche Hashes
        let fp1 = compute_fingerprint("hijack", "192.0.2.0/24", 64512);
        let fp2 = compute_fingerprint("flap", "192.0.2.0/24", 64512);
        let fp3 = compute_fingerprint("hijack", "198.51.100.0/24", 64512);
        let fp4 = compute_fingerprint("hijack", "192.0.2.0/24", 64513);

        assert_ne!(fp1, fp2);
        assert_ne!(fp1, fp3);
        assert_ne!(fp1, fp4);
        assert_ne!(fp2, fp3);
        assert_ne!(fp2, fp4);
        assert_ne!(fp3, fp4);
    }

    #[test]
    fn test_dedup_cache_new_alert() {
        let cache = DedupCache::new();
        let fingerprint = "test_fingerprint_123";

        // Erster Aufruf sollte true zurückgeben (neu)
        assert!(cache.is_new(fingerprint));

        // Zweiter Aufruf sollte false zurückgeben (dupliziert)
        assert!(!cache.is_new(fingerprint));
    }

    #[test]
    fn test_dedup_cache_invalidate() {
        let cache = DedupCache::new();
        let fingerprint = "test_fingerprint_456";

        // Alert ist neu
        assert!(cache.is_new(fingerprint));

        // Alert ist jetzt dupliziert
        assert!(!cache.is_new(fingerprint));

        // Invalidate
        cache.invalidate(fingerprint);

        // Nach Invalidate sollte Alert wieder neu sein
        assert!(cache.is_new(fingerprint));
    }
}
