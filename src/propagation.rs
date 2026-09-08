use dashmap::DashMap;
use ipnet::IpNet;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_stream::StreamExt;

/// Eindeutiger Schlüssel für eine BGP-Ankündigung (unabhängig vom Kollektor).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub prefix: String, // normalisiert: "8.8.8.0/24"
    pub origin_as: u32,
    /// FNV-Hash des AS-Pfads — schnell, kollisionsarm für kurze Vektoren
    pub path_hash: u64,
}

impl GroupKey {
    pub fn from_record(record: &crate::nats_subscriber::BgpRecord) -> Self {
        Self {
            prefix: record.prefix.clone(),
            origin_as: record.origin_as,
            path_hash: path_hash(&record.as_path),
        }
    }
}

/// Eine BGP-Ankündigung die über mehrere Kollektoren beobachtet wurde.
/// Enthält die Ankunftszeiten bei jedem Kollektor als Grundlage
/// für die Wellenphysik-Analyse.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PropagationEvent {
    /// IP-Präfix das angekündigt wurde
    pub prefix: IpNet,
    /// Ursprungs-AS (letzter Eintrag im AS-Pfad)
    pub origin_as: u32,
    /// AS-Pfad der Ankündigung
    pub as_path: Vec<u32>,
    /// Kollektor-ID → Unix-Timestamp (float, sub-second precision)
    /// BTreeMap = sortiert nach Kollektor-ID (deterministisch für Tests)
    pub arrivals: BTreeMap<String, f64>,
    /// Frühester Empfangszeitpunkt (Unix float)
    pub first_arrival: f64,
    /// Spätester Empfangszeitpunkt (Unix float)
    pub last_arrival: f64,
    /// Differenz last_arrival - first_arrival in Millisekunden
    pub spread_ms: f64,
    /// Kollektoren sortiert nach Ankunftszeit (frühester zuerst)
    pub arrival_order: Vec<String>,
}

impl PropagationEvent {
    /// Erstellt ein PropagationEvent und berechnet alle abgeleiteten Felder.
    pub fn new(
        prefix: IpNet,
        origin_as: u32,
        as_path: Vec<u32>,
        arrivals: BTreeMap<String, f64>,
    ) -> Self {
        // Calculate first_arrival, last_arrival
        let first_arrival = arrivals
            .values()
            .copied()
            .fold(f64::INFINITY, |acc, x| acc.min(x));
        let last_arrival = arrivals
            .values()
            .copied()
            .fold(f64::NEG_INFINITY, |acc, x| acc.max(x));

        // Calculate spread in milliseconds
        let spread_ms = (last_arrival - first_arrival) * 1000.0;

        // Create arrival_order: sort collector IDs by timestamp (earliest first)
        let mut arrival_order: Vec<String> = arrivals.keys().cloned().collect();
        arrival_order.sort_by(|a, b| {
            arrivals[a]
                .partial_cmp(&arrivals[b])
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Self {
            prefix,
            origin_as,
            as_path,
            arrivals,
            first_arrival,
            last_arrival,
            spread_ms,
            arrival_order,
        }
    }
}

/// Polynomial hash over an AS path. **The one definition** — every place that
/// keys a propagation group, a baseline entry or a detector lookup by AS path
/// must call this, so a baseline written by one code path is findable by
/// another. (Three separate copies of this hash previously existed; combined
/// with `tools/baseline_builder` storing a truncated `vec![origin_as]` path,
/// baselines were written under a different key than the live detector looked
/// them up under, so live lookups silently never matched.)
pub fn path_hash(as_path: &[u32]) -> u64 {
    as_path.iter().fold(0u64, |acc, &asn| {
        acc.wrapping_mul(31).wrapping_add(asn as u64)
    })
}

/// One observation of an announcement at a single collector — the input unit
/// for building `PropagationEvent`s from archive (MRT) data.
#[derive(Debug, Clone)]
pub struct CollectorObservation {
    pub collector: String,
    pub prefix: String,
    pub origin_as: u32,
    pub as_path: Vec<u32>,
    pub timestamp: f64,
}

/// Builds `PropagationEvent`s from archive observations using the same
/// semantics as the live `PropagationAggregator`: a group is opened by its
/// first arrival and stays open for `window_secs`; a later observation of the
/// same (prefix, origin_as, as_path) starts a *new* group.
///
/// The time window is what makes the result physically meaningful. Without it
/// (as the batch tools previously did) every announcement of a prefix across
/// the whole dataset collapses into one "propagation event", producing spreads
/// of minutes — measured at up to 293 s on real RIS archive data — from
/// announcements that have nothing to do with each other.
///
/// The full `as_path` is carried into the event, not a `vec![origin_as]`
/// stand-in: the path is part of the baseline key the live detector looks up.
pub fn build_events_batch(
    mut observations: Vec<CollectorObservation>,
    window_secs: f64,
    min_collectors: usize,
) -> Vec<PropagationEvent> {
    // Chronological order — sessionization needs to see arrivals in the order
    // they happened, and MRT files are read per collector, not interleaved.
    observations.sort_by(|a, b| {
        a.timestamp
            .partial_cmp(&b.timestamp)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    struct OpenGroup {
        first_ts: f64,
        as_path: Vec<u32>,
        arrivals: BTreeMap<String, f64>,
    }

    let mut open: std::collections::HashMap<(String, u32, u64), OpenGroup> =
        std::collections::HashMap::new();
    let mut events = Vec::new();

    let finish = |key: &(String, u32, u64), group: OpenGroup, out: &mut Vec<PropagationEvent>| {
        if group.arrivals.len() < min_collectors {
            return;
        }
        if let Ok(prefix) = key.0.parse::<IpNet>() {
            out.push(PropagationEvent::new(
                prefix,
                key.1,
                group.as_path,
                group.arrivals,
            ));
        }
    };

    for obs in observations {
        let key = (obs.prefix.clone(), obs.origin_as, path_hash(&obs.as_path));

        if let Some(group) = open.get_mut(&key) {
            if obs.timestamp - group.first_ts <= window_secs {
                group
                    .arrivals
                    .entry(obs.collector)
                    .and_modify(|t| {
                        if obs.timestamp < *t {
                            *t = obs.timestamp;
                        }
                    })
                    .or_insert(obs.timestamp);
                continue;
            }
            // Window elapsed — close this group and open a fresh one below.
            let expired = open.remove(&key).expect("just looked it up");
            finish(&key, expired, &mut events);
        }

        let mut arrivals = BTreeMap::new();
        arrivals.insert(obs.collector, obs.timestamp);
        open.insert(
            key,
            OpenGroup {
                first_ts: obs.timestamp,
                as_path: obs.as_path,
                arrivals,
            },
        );
    }

    for (key, group) in open.drain() {
        finish(&key, group, &mut events);
    }

    events
}

struct PendingGroup {
    arrivals: std::collections::BTreeMap<String, f64>,
    as_path: Vec<u32>,
    created_at: Instant,
}

pub struct PropagationAggregator {
    window: Duration, // 10 Sekunden
    pending: Arc<DashMap<GroupKey, PendingGroup>>,
}

impl PropagationAggregator {
    pub fn new() -> Self {
        Self {
            window: Duration::from_secs(10),
            pending: Arc::new(DashMap::new()),
        }
    }

    /// Fügt ein BgpRecord zum Aggregator hinzu.
    /// Gibt ein fertiges PropagationEvent zurück wenn das Zeitfenster abgelaufen
    /// ist (≥10s seit erstem Arrival dieser Gruppe) — sonst None.
    pub fn add(&self, record: &crate::nats_subscriber::BgpRecord) -> Option<PropagationEvent> {
        let key = GroupKey::from_record(record);
        let timestamp = record.timestamp.timestamp() as f64
            + record.timestamp.timestamp_subsec_nanos() as f64 / 1_000_000_000.0;

        // Check if group already exists
        let mut entry = self
            .pending
            .entry(key.clone())
            .or_insert_with(|| PendingGroup {
                arrivals: BTreeMap::new(),
                as_path: record.as_path.clone(),
                created_at: Instant::now(),
            });

        // Add arrival for this collector
        entry.arrivals.insert(record.collector.clone(), timestamp);

        // Check if window has expired
        if entry.created_at.elapsed() >= self.window {
            // Remove from map and create event
            if let Some((_, pending_group)) = self.pending.remove(&key) {
                let prefix: IpNet = record.prefix.parse().ok()?;
                return Some(PropagationEvent::new(
                    prefix,
                    record.origin_as,
                    pending_group.as_path,
                    pending_group.arrivals,
                ));
            }
        }

        None
    }

    /// Flush: gibt alle Gruppen zurück deren Fenster abgelaufen ist.
    /// Regelmäßig aufrufen (z.B. jede Sekunde) um keine Events zu verlieren.
    pub fn flush_expired(&self) -> Vec<PropagationEvent> {
        let mut events = Vec::new();

        // Collect keys of expired groups
        let expired_keys: Vec<GroupKey> = self
            .pending
            .iter()
            .filter(|entry| entry.created_at.elapsed() >= self.window)
            .map(|entry| entry.key().clone())
            .collect();

        // Remove expired groups and create events
        for key in expired_keys {
            if let Some((_, pending_group)) = self.pending.remove(&key) {
                // Parse prefix - if it fails, skip this event
                if let Ok(prefix) = key.prefix.parse() {
                    let event = PropagationEvent::new(
                        prefix,
                        key.origin_as,
                        pending_group.as_path,
                        pending_group.arrivals,
                    );
                    events.push(event);
                }
            }
        }

        events
    }

    /// Startet den NATS Consumer.
    /// Liest von `bgp.events`, publiziert nach `bgp.propagation`.
    /// Läuft bis der CancellationToken ausgelöst wird.
    pub async fn run(self: Arc<Self>, nats_url: &str) -> Result<(), Box<dyn std::error::Error>> {
        let client = async_nats::connect(nats_url).await?;
        let _js = async_nats::jetstream::new(client.clone());

        let publisher = async_nats::connect(nats_url).await?;
        let js_pub = async_nats::jetstream::new(publisher);

        let mut subscriber = client.subscribe("bgp.events").await?;

        // Flush-Task: alle 1s abgelaufene Fenster publizieren
        let agg_clone = self.clone();
        let js_pub_clone = js_pub.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                for event in agg_clone.flush_expired() {
                    if event.arrivals.len() >= 3 {
                        // ← Mindest-Schwelle
                        if let Ok(bytes) = serde_json::to_vec(&event) {
                            let _ = js_pub_clone.publish("bgp.propagation", bytes.into()).await;
                        }
                    }
                }
            }
        });

        // Haupt-Loop: eingehende Events verarbeiten
        while let Some(msg) = subscriber.next().await {
            if let Ok(bgp_event) =
                serde_json::from_slice::<crate::nats_subscriber::BgpEvent>(&msg.payload)
            {
                if let Some(record) = crate::nats_subscriber::extract_bgp_record(&bgp_event) {
                    if let Some(prop_event) = self.add(&record) {
                        if prop_event.arrivals.len() >= 3 {
                            // ← Mindest-Schwelle
                            if let Ok(bytes) = serde_json::to_vec(&prop_event) {
                                let _ = js_pub.publish("bgp.propagation", bytes.into()).await;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Default for PropagationAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    fn obs(
        collector: &str,
        prefix: &str,
        origin: u32,
        path: &[u32],
        ts: f64,
    ) -> CollectorObservation {
        CollectorObservation {
            collector: collector.to_string(),
            prefix: prefix.to_string(),
            origin_as: origin,
            as_path: path.to_vec(),
            timestamp: ts,
        }
    }

    /// Regression: the batch builders used to group over the whole dataset
    /// with no time window, so two announcements minutes apart collapsed into
    /// a single event with a nonsense spread (up to 293 s on real archive
    /// data). They must come out as two separate events.
    #[test]
    fn test_build_events_batch_splits_on_window() {
        let path = [1u32, 2, 15169];
        let observations = vec![
            obs("rrc00", "8.8.8.0/24", 15169, &path, 1000.0),
            obs("rrc11", "8.8.8.0/24", 15169, &path, 1000.5),
            obs("rrc12", "8.8.8.0/24", 15169, &path, 1001.0),
            // 60 s later — a separate announcement, not the same wave.
            obs("rrc00", "8.8.8.0/24", 15169, &path, 1060.0),
            obs("rrc11", "8.8.8.0/24", 15169, &path, 1060.4),
            obs("rrc12", "8.8.8.0/24", 15169, &path, 1060.8),
        ];

        let mut events = build_events_batch(observations, 10.0, 3);
        events.sort_by(|a, b| a.first_arrival.partial_cmp(&b.first_arrival).unwrap());

        assert_eq!(events.len(), 2, "60 s apart must not merge into one event");
        assert!(
            events[0].spread_ms < 1100.0 && events[1].spread_ms < 1100.0,
            "each event's spread must stay inside the window, got {:?}",
            events.iter().map(|e| e.spread_ms).collect::<Vec<_>>()
        );
    }

    /// Regression: the batch builders replaced the AS path with
    /// `vec![origin_as]`, so baseline entries were keyed by a hash the live
    /// detector never computes. The full path must survive into the event.
    #[test]
    fn test_build_events_batch_preserves_full_as_path() {
        let path = [64496u32, 64497, 15169];
        let observations = vec![
            obs("rrc00", "8.8.8.0/24", 15169, &path, 10.0),
            obs("rrc11", "8.8.8.0/24", 15169, &path, 10.2),
            obs("rrc12", "8.8.8.0/24", 15169, &path, 10.4),
        ];

        let events = build_events_batch(observations, 10.0, 3);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_path, path.to_vec());
        assert_eq!(path_hash(&events[0].as_path), path_hash(&path));
    }

    #[test]
    fn test_build_events_batch_drops_below_min_collectors() {
        let path = [1u32, 15169];
        let observations = vec![
            obs("rrc00", "8.8.8.0/24", 15169, &path, 10.0),
            obs("rrc11", "8.8.8.0/24", 15169, &path, 10.2),
        ];
        assert!(build_events_batch(observations, 10.0, 3).is_empty());
    }

    /// Different AS paths for the same prefix are different waves and must not
    /// share a group — that is what the path component of the key is for.
    #[test]
    fn test_build_events_batch_separates_distinct_paths() {
        let observations = vec![
            obs("rrc00", "8.8.8.0/24", 15169, &[1, 15169], 10.0),
            obs("rrc11", "8.8.8.0/24", 15169, &[1, 15169], 10.2),
            obs("rrc12", "8.8.8.0/24", 15169, &[1, 15169], 10.4),
            obs("rrc00", "8.8.8.0/24", 15169, &[2, 3, 15169], 10.1),
            obs("rrc11", "8.8.8.0/24", 15169, &[2, 3, 15169], 10.3),
            obs("rrc12", "8.8.8.0/24", 15169, &[2, 3, 15169], 10.5),
        ];
        assert_eq!(build_events_batch(observations, 10.0, 3).len(), 2);
    }

    /// The batch path and the live `PropagationAggregator` must key groups
    /// identically — a baseline built from archives is only useful if the live
    /// detector looks it up under the same hash.
    #[test]
    fn test_batch_and_live_group_keys_agree() {
        let as_path = vec![64496u32, 64497, 15169];
        let record = crate::nats_subscriber::BgpRecord {
            prefix: "8.8.8.0/24".to_string(),
            origin_as: 15169,
            as_path: as_path.clone(),
            timestamp: Utc.timestamp_opt(1000, 0).unwrap(),
            event_type: "announce".to_string(),
            collector: "rrc00".to_string(),
            peer_asn: 1,
            peer_ip: "192.0.2.1".to_string(),
        };
        assert_eq!(
            GroupKey::from_record(&record).path_hash,
            path_hash(&as_path)
        );
    }

    fn make_arrivals(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    /// Helper to create a BgpRecord for testing
    fn make_record(
        collector: &str,
        prefix: &str,
        as_path: &[u32],
    ) -> crate::nats_subscriber::BgpRecord {
        let timestamp = Utc.timestamp_opt(1000, 0).unwrap(); // arbitrary timestamp
        crate::nats_subscriber::BgpRecord {
            prefix: prefix.to_string(),
            origin_as: *as_path.last().unwrap_or(&0),
            peer_asn: *as_path.first().unwrap_or(&0),
            event_type: "announce".to_string(),
            as_path: as_path.to_vec(),
            timestamp,
            collector: collector.to_string(),
            peer_ip: "10.0.0.1".to_string(),
        }
    }

    #[test]
    fn test_propagation_event_spread_and_order() {
        let arrivals = make_arrivals(&[
            ("rrc12", 1000.000), // Frankfurt — zuerst
            ("rrc00", 1000.089), // Amsterdam
            ("rrc11", 1000.891), // New York
            ("rrc17", 1001.234), // Singapore — zuletzt
        ]);
        let event = PropagationEvent::new(
            "8.8.8.0/24".parse().unwrap(),
            15169,
            vec![1103, 3356, 15169],
            arrivals,
        );

        assert!((event.spread_ms - 1234.0).abs() < 1.0);
        assert_eq!(event.arrival_order[0], "rrc12");
        assert_eq!(event.arrival_order[3], "rrc17");
        assert_eq!(event.first_arrival, 1000.000);
        assert_eq!(event.last_arrival, 1001.234);
    }

    #[test]
    fn test_propagation_event_single_arrival() {
        let arrivals = make_arrivals(&[("rrc12", 1000.500)]);
        let event = PropagationEvent::new("1.0.0.0/8".parse().unwrap(), 1, vec![1], arrivals);
        assert!(event.spread_ms < 0.001);
        assert_eq!(event.arrival_order.len(), 1);
    }

    #[test]
    fn test_aggregator_collects_multiple_collectors() {
        use crate::nats_subscriber::BgpRecord;
        let agg = PropagationAggregator::new();

        let make_record = |collector: &str, ts: f64| BgpRecord {
            prefix: "8.8.8.0/24".to_string(),
            origin_as: 15169,
            peer_asn: 1103,
            event_type: "announce".to_string(),
            as_path: vec![1103, 15169],
            timestamp: chrono::DateTime::from_timestamp(ts as i64, 0).unwrap(),
            collector: collector.to_string(),
            peer_ip: "10.0.0.1".to_string(),
        };

        // Drei Kollektoren, gleiche Gruppe
        let r1 = agg.add(&make_record("rrc12", 1000.0));
        let r2 = agg.add(&make_record("rrc00", 1000.089));
        let r3 = agg.add(&make_record("rrc11", 1000.891));

        // Noch kein Event — Fenster nicht abgelaufen
        assert!(r1.is_none());
        assert!(r2.is_none());
        assert!(r3.is_none());

        // Noch in der Map
        assert_eq!(agg.pending.len(), 1);
    }

    #[test]
    fn test_aggregator_different_prefix_different_group() {
        use crate::nats_subscriber::BgpRecord;
        let agg = PropagationAggregator::new();

        let make = |prefix: &str, collector: &str| BgpRecord {
            prefix: prefix.to_string(),
            origin_as: 15169,
            peer_asn: 1103,
            event_type: "announce".to_string(),
            as_path: vec![1103, 15169],
            timestamp: chrono::Utc::now(),
            collector: collector.to_string(),
            peer_ip: "".to_string(),
        };

        agg.add(&make("8.8.8.0/24", "rrc12"));
        agg.add(&make("1.1.1.0/24", "rrc12")); // anderes Prefix → andere Gruppe

        assert_eq!(agg.pending.len(), 2);
    }

    #[test]
    fn test_min_threshold_2_collectors_no_event() {
        // 2 Kollektoren → PropagationEvent wird erstellt aber hat arrivals.len() == 2
        // → sollte NICHT nach bgp.propagation publiziert werden
        // Hier testen wir nur dass spread_ms und arrival_order korrekt sind
        // (Die Publikations-Logik ist in run() — hier nur die Struktur testen)
        let arrivals = make_arrivals(&[("rrc12", 1000.0), ("rrc00", 1000.1)]);
        let event = PropagationEvent::new("1.0.0.0/24".parse().unwrap(), 1, vec![1], arrivals);
        assert_eq!(event.arrivals.len(), 2);
        // 2 < 3 → wäre nicht publiziert worden
        // Use epsilon comparison for floating point
        assert!((event.spread_ms - 100.0).abs() < 0.001); // (1000.1 - 1000.0) * 1000 = 100.0
        assert_eq!(event.arrival_order, vec!["rrc12", "rrc00"]);
    }

    #[test]
    fn test_group_key_same_path_same_group() {
        let r1 = make_record("rrc12", "8.8.8.0/24", &[1103, 15169]);
        let r2 = make_record("rrc00", "8.8.8.0/24", &[1103, 15169]);
        assert_eq!(GroupKey::from_record(&r1), GroupKey::from_record(&r2));
    }

    #[test]
    fn test_group_key_different_path_different_group() {
        let r1 = make_record("rrc12", "8.8.8.0/24", &[1103, 15169]);
        let r2 = make_record("rrc12", "8.8.8.0/24", &[3356, 15169]); // anderer Pfad
        assert_ne!(GroupKey::from_record(&r1), GroupKey::from_record(&r2));
    }

    #[test]
    fn test_arrival_order_correct_sorting() {
        let arrivals = make_arrivals(&[
            ("rrc17", 1001.234), // Singapore — späteste
            ("rrc11", 1000.891), // New York
            ("rrc12", 1000.000), // Frankfurt — früheste
            ("rrc00", 1000.089), // Amsterdam
        ]);
        let event =
            PropagationEvent::new("8.8.8.0/24".parse().unwrap(), 15169, vec![15169], arrivals);
        assert_eq!(
            event.arrival_order,
            vec!["rrc12", "rrc00", "rrc11", "rrc17"]
        );
        assert_eq!(event.arrival_order[0], "rrc12"); // Frankfurt zuerst
        assert_eq!(event.arrival_order[3], "rrc17"); // Singapore zuletzt
    }
}
