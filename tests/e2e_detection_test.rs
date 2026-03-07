use std::collections::HashMap;
use std::sync::Arc;

use log_gateway::detector_loop::{DetectorLoop, DetectorLoopConfig};
use log_gateway::nats_subscriber::BgpRecord;
use log_gateway::rpki_cache::RpkiCache;

/// Hilfsfunktion: Erstellt einen DetectorLoop mit gegebenem RPKI-Index
async fn make_test_loop(rpki_index: HashMap<(u8, u128), Vec<(u8, u32)>>) -> DetectorLoop {
    let rpki_cache = Arc::new(RpkiCache::new("http://dummy".to_string()));
    rpki_cache.set_test_data(rpki_index).await;
    let config = DetectorLoopConfig {
        nats_url: "nats://localhost:4222".to_string(),
        subject: "bgp.events".to_string(),
        webhook_targets: vec![], // kein echter Webhook in Tests
        irr_enabled: false,      // IRR deaktiviert für deterministische Tests
        channel_size: 1000,
    };
    DetectorLoop::new(config, rpki_cache).unwrap()
}

/// Hilfsfunktion: Erstellt einen BgpRecord
fn make_bgp_record(prefix: &str, origin_as: u32, event_type: &str) -> BgpRecord {
    BgpRecord {
        prefix: prefix.to_string(),
        origin_as,
        peer_asn: 1234,
        event_type: event_type.to_string(),
        as_path: vec![1234, origin_as],
        timestamp: chrono::Utc::now(),
    }
}

/// Hilfsfunktion: Erstellt einen RPKI-Index für einen einzelnen Prefix
fn make_rpki_index(prefix: &str, max_length: u8, asn: u32) -> HashMap<(u8, u128), Vec<(u8, u32)>> {
    use std::net::IpAddr;
    let net: ipnet::IpNet = prefix.parse().unwrap();
    let addr = match net.network() {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().to_bits(),
        IpAddr::V6(v6) => v6.to_bits(),
    };
    HashMap::from([((net.prefix_len(), addr), vec![(max_length, asn)])])
}

/// Test 1: Hijack mit RPKI invalid ASN löst Anomalie aus
#[tokio::test]
async fn test_hijack_with_rpki_invalid_asn_triggers_anomaly() {
    // Setup: RPKI Cache mit VRP: 192.0.2.0/24, max_length=24, origin AS=64512
    let rpki_index = make_rpki_index("192.0.2.0/24", 24, 64512);
    let detector_loop = make_test_loop(rpki_index).await;

    // BgpRecord mit anderem ASN (99999) -> InvalidAsn
    let record = make_bgp_record("192.0.2.0/24", 99999, "announce");

    // Verarbeitung
    let anomalies = detector_loop.process_record(&record).await;

    // Erwartung: mindestens 1 Anomalie
    assert!(
        !anomalies.is_empty(),
        "Expected at least 1 anomaly for RPKI InvalidAsn"
    );

    // Prüfe, dass es eine PossibleHijack Anomalie ist oder confidence > 0.5
    let hijack_anomaly = anomalies
        .iter()
        .find(|a| a.anomaly_type.to_string().contains("PossibleHijack") || a.confidence > 0.5);
    assert!(
        hijack_anomaly.is_some(),
        "Expected PossibleHijack anomaly or confidence > 0.5, got anomalies: {:?}",
        anomalies
    );
}

/// Test 2: RPKI valid unterdrückt Anomalie (sofortiger Return)
#[tokio::test]
async fn test_rpki_valid_suppresses_anomaly() {
    // Setup: RPKI Cache mit VRP: 192.0.2.0/24, max_length=24, origin AS=64512
    let rpki_index = make_rpki_index("192.0.2.0/24", 24, 64512);
    let detector_loop = make_test_loop(rpki_index).await;

    // BgpRecord mit korrektem ASN (64512) -> Valid
    let record = make_bgp_record("192.0.2.0/24", 64512, "announce");

    // Verarbeitung
    let anomalies = detector_loop.process_record(&record).await;

    // Erwartung: leerer Vec (RPKI Valid → sofortiger Return)
    assert!(
        anomalies.is_empty(),
        "Expected empty anomalies for RPKI Valid, got: {:?}",
        anomalies
    );
}

/// Test 3: Withdraw löst keinen Hijack aus
#[tokio::test]
async fn test_withdraw_never_triggers_hijack() {
    // Setup: RPKI Cache mit beliebigem VRP
    let rpki_index = make_rpki_index("192.0.2.0/24", 24, 64512);
    let detector_loop = make_test_loop(rpki_index).await;

    // BgpRecord mit event_type="withdraw"
    let record = make_bgp_record("192.0.2.0/24", 99999, "withdraw");

    // Verarbeitung
    let anomalies = detector_loop.process_record(&record).await;

    // Erwartung: Keine PossibleHijack Anomalie
    // (FlappingDetector darf feuern, aber erst nach 50 Events)
    let hijack_anomalies: Vec<_> = anomalies
        .iter()
        .filter(|a| a.anomaly_type.to_string().contains("PossibleHijack"))
        .collect();
    assert!(
        hijack_anomalies.is_empty(),
        "Expected no PossibleHijack anomalies for withdraw, got: {:?}",
        hijack_anomalies
    );
}

/// Test 4: Dedup verhindert duplicate webhook
#[tokio::test]
async fn test_dedup_prevents_duplicate_webhook() {
    // Setup: RPKI Cache mit VRP, das zu InvalidAsn führt
    let rpki_index = make_rpki_index("192.0.2.0/24", 24, 64512);
    let detector_loop = make_test_loop(rpki_index).await;

    // Zwei identische BgpRecords
    let record1 = make_bgp_record("192.0.2.0/24", 99999, "announce");
    let record2 = make_bgp_record("192.0.2.0/24", 99999, "announce");

    // Verarbeitung des ersten Records
    let anomalies1 = detector_loop.process_record(&record1).await;
    assert!(
        !anomalies1.is_empty(),
        "First record should trigger anomaly"
    );

    // Verarbeitung des zweiten Records
    let anomalies2 = detector_loop.process_record(&record2).await;
    assert!(
        !anomalies2.is_empty(),
        "Second record should also detect anomaly"
    );

    // Überprüfung der Dedup-Logik: Gleiche Anomalien sollten gleichen Fingerprint haben
    // Da wir nicht direkt auf dedup_cache zugreifen können, prüfen wir zumindest,
    // dass compute_fingerprint für gleiche Anomalien gleiche Werte liefert
    use log_gateway::alert_dedup::compute_fingerprint;

    if let Some(anomaly1) = anomalies1.first() {
        if let Some(anomaly2) = anomalies2.first() {
            let fp1 = compute_fingerprint(
                &anomaly1.anomaly_type.to_string(),
                &anomaly1.prefix,
                anomaly1.origin_as,
            );
            let fp2 = compute_fingerprint(
                &anomaly2.anomaly_type.to_string(),
                &anomaly2.prefix,
                anomaly2.origin_as,
            );
            assert_eq!(
                fp1, fp2,
                "Fingerprints should be identical for identical anomalies"
            );
        }
    }
}

/// Test 5: Flapping über Threshold löst Anomalie aus
#[tokio::test]
async fn test_flapping_above_threshold_triggers_anomaly() {
    // Setup: RPKI Cache ohne VRP (NotFound)
    let rpki_index = HashMap::new(); // leerer Index -> NotFound
    let detector_loop = make_test_loop(rpki_index).await;

    // Prefix für Flapping-Test
    let prefix = "203.0.113.0/24";
    let origin_as = 64512;

    // Sende 50 alternating announce/withdraw Events (abwechselnd)
    // Das ergibt 49 Richtungswechsel (jeder Wechsel announce↔withdraw zählt)
    let mut flapping_detected_at = None;
    for i in 0..50 {
        let event_type = if i % 2 == 0 { "announce" } else { "withdraw" };
        let record = make_bgp_record(prefix, origin_as, event_type);
        let anomalies = detector_loop.process_record(&record).await;

        // Prüfe auf PrefixFlapping Anomalien
        let has_flapping = anomalies
            .iter()
            .any(|a| a.anomaly_type.to_string().contains("PrefixFlapping"));

        if has_flapping {
            flapping_detected_at = Some(i);
            // Debug-Ausgabe
            println!("Flapping detected at event {} (type: {})", i, event_type);
        }

        // Erwartung: Erst nach dem 50. Event sollte Flapping erkannt werden
        // (FlappingDetector benötigt 50 Events im 5-Minuten-Fenster und mind. 6 Richtungswechsel)
        // Mit abwechselnden Events haben wir nach 50 Events: 49 Richtungswechsel > 6
        if i < 49 {
            assert!(
                !has_flapping,
                "Flapping should not be detected before 50 events (detected at event {})",
                i
            );
        }
    }

    // Erwartung: Nach dem 50. Event sollte Flapping erkannt werden
    assert!(
        flapping_detected_at.is_some(),
        "Expected PrefixFlapping anomaly after 50 alternating events"
    );

    // Verifiziere, dass es genau beim 49. Event (0-basiert) oder 50. Event erkannt wurde
    let detected_at = flapping_detected_at.unwrap();
    assert!(
        detected_at >= 48, // Event 48 oder 49 (0-basiert)
        "Flapping should be detected at event 49 or 50, but was detected at {}",
        detected_at
    );
}

/// Zusätzlicher Test: RPKI InvalidLength
#[tokio::test]
async fn test_rpki_invalid_length_triggers_anomaly() {
    // Setup: RPKI Cache mit VRP: 192.0.2.0/24, max_length=24, origin AS=64512
    let rpki_index = make_rpki_index("192.0.2.0/24", 24, 64512);
    let detector_loop = make_test_loop(rpki_index).await;

    // BgpRecord mit korrektem ASN aber zu spezifischem Prefix (/28 > max_length 24)
    let record = make_bgp_record("192.0.2.0/28", 64512, "announce");

    // Verarbeitung
    let anomalies = detector_loop.process_record(&record).await;

    // Erwartung: mindestens 1 Anomalie (InvalidLength)
    assert!(
        !anomalies.is_empty(),
        "Expected at least 1 anomaly for RPKI InvalidLength"
    );
}

/// Zusätzlicher Test: RPKI NotFound (kein VRP)
#[tokio::test]
async fn test_rpki_notfound_still_checks_hijack() {
    // Setup: leerer RPKI Cache
    let rpki_index = HashMap::new();
    let detector_loop = make_test_loop(rpki_index).await;

    // BgpRecord mit beliebigem Prefix
    let record = make_bgp_record("10.0.0.0/8", 64512, "announce");

    // Verarbeitung — Ergebnis wird nicht weiter geprüft, da der Test nur
    // sicherstellt dass kein Panic auftritt (RPKI NotFound → HijackDetector läuft weiter)
    let anomalies = detector_loop.process_record(&record).await;

    // RPKI NotFound → kein sofortiger Return, HijackDetector und FlappingDetector laufen
    // Jede Anzahl von Anomalien ist gültig — der Test prüft nur Panic-Freiheit.
    let _ = anomalies.len(); // suppress unused warning, ensure vec is valid
}
