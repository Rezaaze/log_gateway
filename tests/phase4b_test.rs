// Integration test for Phase 4b Implementation
// Tests DetectorRunner integration with NATS and metrics

use chrono::Utc;
use log_gateway::detector_runner::DetectorRunner;
use log_gateway::nats_subscriber::BgpRecord;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::test]
async fn test_detector_runner_integration() {
    println!("=== Testing Phase 4b Implementation ===");

    // Zero learning period so the run below can establish a prefix's normal
    // origin with one announcement and have the next, different origin count
    // as an event straight away. In production the learning period gives the
    // detector time to collect a prefix's legitimate origins first.
    let hijack_detector = Arc::new(
        log_gateway::anomaly_detector::HijackDetector::new()
            .with_learning_period(chrono::Duration::zero()),
    );
    let detector_runner = Arc::new(DetectorRunner::new().with_hijack_detector(hijack_detector));

    // Create a test channel
    let (tx, rx) = mpsc::channel::<BgpRecord>(10);

    // Clone for the async task
    let runner_clone = Arc::clone(&detector_runner);

    // Spawn the detector runner task
    let handle = tokio::spawn(async move {
        runner_clone.run(rx).await;
    });

    // Send some test BGP records
    println!("Sending test BGP records...");

    // Test 1: Normal announcement
    let record1 = BgpRecord {
        timestamp: Utc::now(),
        event_type: "announce".to_string(),
        prefix: "192.168.1.0/24".to_string(),
        origin_as: 65001,
        as_path: vec![65001],
        peer_asn: 65000,
        collector: "unknown".to_string(),
        peer_ip: "".to_string(),
    };

    tx.send(record1).await.unwrap();
    println!("  - Sent normal announcement");

    // Test 2: Potential hijack (using test prefixes)
    let record2 = BgpRecord {
        timestamp: Utc::now(),
        event_type: "announce".to_string(),
        prefix: "192.0.2.0/24".to_string(), // Test prefix that triggers simulated hijack
        origin_as: 65002,
        as_path: vec![65002],
        peer_asn: 65000,
        collector: "unknown".to_string(),
        peer_ip: "".to_string(),
    };

    tx.send(record2).await.unwrap();
    println!("  - Sent potential hijack (test prefix)");

    // Test 3: Flapping detection (long AS path)
    let record3 = BgpRecord {
        timestamp: Utc::now(),
        event_type: "announce".to_string(),
        prefix: "10.0.0.0/8".to_string(),
        origin_as: 65003,
        as_path: vec![
            65003, 65004, 65005, 65006, 65007, 65008, 65009, 65010, 65011, 65012, 65013,
        ],
        peer_asn: 65000,
        collector: "unknown".to_string(),
        peer_ip: "".to_string(),
    };

    tx.send(record3).await.unwrap();
    println!("  - Sent announcement with long AS path (potential flapping)");

    // Test 4: Withdrawal
    let record4 = BgpRecord {
        timestamp: Utc::now(),
        event_type: "withdraw".to_string(),
        prefix: "192.168.2.0/24".to_string(),
        origin_as: 65004,
        as_path: vec![65004],
        peer_asn: 65000,
        collector: "unknown".to_string(),
        peer_ip: "".to_string(),
    };

    tx.send(record4).await.unwrap();
    println!("  - Sent withdrawal");

    // Test 5: The actual hijack signature — a prefix whose normal origin the
    // runner has already observed (record3, AS65003) is now announced by a
    // different AS. Records 1–4 each concerned a prefix the runner had never
    // seen; those establish state and must stay silent, which is why this
    // fifth record is what the anomaly count below refers to.
    let record5 = BgpRecord {
        timestamp: Utc::now(),
        event_type: "announce".to_string(),
        prefix: "10.0.0.0/8".to_string(),
        origin_as: 65099,
        as_path: vec![65099],
        peer_asn: 65000,
        collector: "unknown".to_string(),
        peer_ip: "".to_string(),
    };

    tx.send(record5).await.unwrap();
    println!("  - Sent new origin for an already-observed prefix");

    // Drop the sender to signal completion
    drop(tx);

    // Wait for the detector runner to finish
    handle.await.unwrap();

    // Get stats
    let stats = detector_runner.stats();
    println!("\n=== DetectorRunner Statistics ===");
    println!("Events processed: {}", stats.events_processed);
    println!("Anomalies detected: {}", stats.anomalies_detected);
    println!("Errors: {}", stats.errors);

    // Verify the stats
    assert_eq!(stats.events_processed, 5);
    // Exactly one anomaly: record5's new origin for a prefix whose normal
    // origin was already observed. This assertion used to read 3 — one for
    // each announced prefix — which encoded the cold-start bug: every prefix
    // seen for the first time was reported as a hijack, so a restart flagged
    // the whole visible routing table.
    assert_eq!(stats.anomalies_detected, 1);
    assert_eq!(stats.errors, 0);

    // Get metrics
    let metrics = detector_runner.gather_metrics();
    println!("\n=== Prometheus Metrics ===");
    println!("{}", metrics);

    // Verify metrics contain expected data
    assert!(metrics.contains("detector_events_processed_total"));
    assert!(metrics.contains("detector_anomalies_detected_total"));
    assert!(metrics.contains("detector_processing_errors_total"));

    println!("\n=== Phase 4b Implementation Test Complete ===");
    println!("✓ DetectorRunner successfully integrated");
    println!("✓ NATS subscription channel working");
    println!("✓ Anomaly detection simulation working");
    println!("✓ Metrics export working");
}
