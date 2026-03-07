# Streaming-Only Architecture Migration Roadmap

**Status**: Decision Made (Option F - Streaming-Only)
**Start Date**: 2026-03-05
**Target Completion**: 2026-03-19 (2 weeks)
**Owner**: Development Team

---

## 1. Executive Summary

### Current Problem
- **ClickHouse Storage**: 24GB/day @ 14,600 events/sec
- **Available Storage**: 56GB (fills in 2-3 days)
- **90-day TTL Requirement**: 1.7TB (impossible on current hardware)
- **Cost**: €400-600 for disk expansion + annual maintenance

### Proposed Solution
**Migrate from ClickHouse DB to NATS Jetstream Streaming Architecture**

```
Old Flow:  bgp-stream → HAProxy/Gateway → ClickHouse DB → Anomaly Detectors
New Flow:  bgp-stream → NATS Jetstream → Anomaly Detectors (Real-time) → Prometheus → Grafana
```

### Expected Benefits
| Metric | Current | Target | Improvement |
|--------|---------|--------|-------------|
| Daily Storage | 24GB | 3GB | 87% reduction |
| Peak Storage (90d) | 1.7TB | 270GB | 84% reduction |
| Real-time Latency | 5-10s batch | <100ms | 50-100x faster |
| Hardware Cost | €400-600 | €0 | 100% savings |
| Setup Complexity | High (DB tuning) | Low (NATS config) | Simplified |

---

## 2. Current Architecture State

### Services Running (as of 2026-03-05)
```
✅ HAProxy (8090)          - Load balancer (4 gateways)
✅ Gateway 1-4 (8080)      - Log ingestion servers
❌ BGP-Stream             - STOPPED (filling disk)
✅ ClickHouse (8123)       - 75GB/75GB FULL (100%)
✅ Prometheus (9090)       - Metrics collection
✅ Grafana (3001)          - Dashboards
✅ Routinator (8323)       - RPKI validation
✅ Alertmanager (9093)     - Alert routing
✅ MinIO (9000/9002)       - S3-compatible storage

Volumes Breakdown:
├─ /var/lib/docker/containers:  60GB (ClickHouse layer)
├─ /var/lib/docker/volumes:     8.7GB (actual data)
│  ├─ clickhouse-data:          4.1GB
│  ├─ routinator-data:          2.6GB
│  ├─ clickhouse-logs:          1.9GB
│  └─ other:                    0.1GB
└─ /usr:                        2.2GB
```

### Code Structure
```
log-gateway/
├── src/
│   ├── main.rs                      # Gateway HTTP server
│   ├── clickhouse_exporter.rs       # ClickHouse sink (TO REMOVE)
│   ├── rpki_cache.rs                # RPKI validation (KEEP)
│   ├── gateway.rs                   # Request handling (MODIFY)
│   └── anomaly_detection/
│       ├── hijack_detector.rs       # Uses ClickHouse (REWRITE)
│       ├── flapping_detector.rs     # Uses ClickHouse (REWRITE)
│       └── mod.rs
├── tools/bgp_stream/
│   └── src/
│       ├── main.rs                  # BGP streaming (REWRITE)
│       ├── gateway_client.rs        # Gateway HTTP client (REMOVE)
│       └── websocket.rs             # RIPE RIS connection (KEEP)
├── deploy/
│   ├── clickhouse/
│   │   ├── init.sql                 # Schema (REMOVE)
│   │   └── alert_rules_schema.sql   (REMOVE)
│   ├── prometheus.prod.yml          # Scrape config (MODIFY)
│   ├── prometheus.rules.yml         # Alert rules (KEEP)
│   └── haproxy/
│       └── haproxy.cfg              # Load balancer (MODIFY)
└── docker-compose.prod.yml          # Services (MODIFY)
```

---

## 3. Architecture Design

### New Flow Diagram
```
┌─────────────────────────────────────────────────────────────┐
│                     Internet (BGP Traffic)                  │
└────────────────────────┬────────────────────────────────────┘
                         │
        ┌────────────────▼────────────────┐
        │   RIPE NCC RIS Live WebSocket   │
        └────────────────┬────────────────┘
                         │
        ┌────────────────▼────────────────────────┐
        │    BGP-Stream (Rust)                    │
        │  - Subscribe to RIS Live                │
        │  - Parse BGP UPDATE/WITHDRAW            │
        │  - NATS Publisher (nats://bgp-events)   │
        └────────────────┬────────────────────────┘
                         │ NATS Publish
        ┌────────────────▼──────────────────────────────────┐
        │         NATS Jetstream                            │
        │    ├─ Subject: bgp.events                         │
        │    ├─ Retention: 24h (configurable)               │
        │    ├─ Storage: 50MB (vs 24GB/day)                 │
        │    └─ Consumer Groups: detector-group             │
        └────────────┬──────────────┬──────────────────────┘
                     │              │
      ┌──────────────▼──┐    ┌─────▼──────────────┐
      │ HijackDetector  │    │ FlappingDetector   │
      │ - NATS Sub      │    │ - NATS Sub         │
      │ - Real-time     │    │ - 5min window      │
      │ - 7d Baseline   │    │ - Real-time        │
      │ - Write Anomaly │    │ - Write Anomaly    │
      │   to Prometheus │    │   to Prometheus    │
      └────────┬────────┘    └─────┬──────────────┘
               │                   │
      ┌────────▼───────────────────▼────────┐
      │    Prometheus                        │
      │  - Scrape metrics from detectors     │
      │  - Store time-series (1GB/15d)       │
      │  - Alert rules evaluation            │
      └────────┬────────────────────────────┘
               │
      ┌────────▼───────────────────────────┐
      │   Grafana                           │
      │  - Query Prometheus (PromQL)        │
      │  - Dashboards & Alerts              │
      │  - User Interface                   │
      └─────────────────────────────────────┘

Optional: Archive Service
      ┌────────────────────────────────────┐
      │  Archive Service (Daily Batch)      │
      │  - Subscribe to nats://bgp-events   │
      │  - Compress & Upload to S3/MinIO    │
      │  - Retention: 90 days               │
      └────────────────────────────────────┘
```

### Storage Tiers (Optional)
```
Hot Tier (ClickHouse/NATS):
├─ Retention: 24h (real-time anomaly detection)
├─ Storage: 50MB (NATS buffer only)
└─ Access: <100ms

Warm Tier (MinIO):
├─ Retention: 30 days
├─ Storage: Compressed (~5GB)
└─ Access: <1s

Cold Tier (S3):
├─ Retention: 90 days (archive)
├─ Storage: Compressed (~30GB)
└─ Access: <10s
```

---

## 4. Phase Breakdown

### Phase 1: Setup Infrastructure (2 days)
**Goal**: Add NATS Jetstream to Docker, verify connectivity

#### 1.1 NATS Jetstream Docker Integration
- [ ] Add `nats:latest` service to docker-compose.prod.yml
  - Port: 4222 (NATS protocol)
  - Port: 8222 (HTTP management)
  - Storage: /nats/jetstream (50MB default)
  - Config: Enable Jetstream module

- [ ] Create `deploy/nats/nats.conf`
  ```
  jetstream {
    store_dir: /data/jetstream
    max_mem_store: 1G
    max_file_store: 10G
  }
  ```

- [ ] Test NATS connectivity
  - `nats --server=nats://nats:4222 stream add bgp-stream`
  - Verify subject: `bgp.events`
  - Consumer group: `detector-group`

#### 1.2 Add Cargo Dependencies
- [ ] Add `async-nats` to `Cargo.toml` (bgp_stream)
  ```toml
  async-nats = "0.34"
  ```

- [ ] Add metrics export dependencies
  ```toml
  prometheus = "0.13"
  ```

**Effort**: 1-2 days
**Risk**: NATS cluster setup complexity (mitigated: single-node setup)
**Deliverable**: `docker-compose.prod.yml` with NATS, passing connectivity test

---

### Phase 2: BGP-Stream Rewrite (2-3 days)
**Goal**: bgp-stream publishes to NATS instead of Gateway

#### 2.1 Remove Gateway Client
- [ ] Delete `tools/bgp_stream/src/gateway_client.rs`
- [ ] Remove HTTP client logic from `tools/bgp_stream/src/main.rs`
- [ ] Remove gateway_client dependencies from Cargo.toml

#### 2.2 Implement NATS Publisher
- [ ] Create `tools/bgp_stream/src/nats_publisher.rs`
  ```rust
  pub struct NatsPublisher {
      client: async_nats::Client,
      subject: String,
  }

  impl NatsPublisher {
      pub async fn connect(url: &str) -> Result<Self> {
          let client = async_nats::connect(url).await?;
          Ok(Self {
              client,
              subject: "bgp.events".to_string(),
          })
      }

      pub async fn publish(&self, events: Vec<BgpEvent>) -> Result<()> {
          for event in events {
              let json = serde_json::to_vec(&event)?;
              self.client.publish(self.subject.clone(), json.into()).await?;
          }
          Ok(())
      }
  }
  ```

- [ ] Update `tools/bgp_stream/src/main.rs`
  - Replace gateway_client with nats_publisher
  - Change env var: `GATEWAY_URL` → `NATS_URL`
  - Update batching logic (now to NATS instead of HTTP)

- [ ] Environment variables (docker-compose)
  ```yaml
  bgp-stream:
    environment:
      NATS_URL: "nats://bgp-stream:bgp-stream@localhost:4222"
      # network_mode: host: Docker-DNS nicht verfügbar → localhost statt nats
      # Credentials: BGP-Account aus deploy/nats/nats.conf (bgp-stream:bgp-stream)
      BATCH_SIZE: "1000"  # Can be larger now (no HTTP overhead)
      BATCH_TIMEOUT_MS: "10"  # Can be faster (NATS is local)
  ```

#### 2.3 Testing
- [ ] Unit tests for NatsPublisher
- [ ] Integration test: bgp-stream → NATS → verify messages
- [ ] Load test: 14,600 events/sec throughput

**Effort**: 2-3 days
**Risk**: NATS publish failures → need retry logic
**Deliverable**: bgp-stream binary publishing to NATS, 0 errors

---

### Phase 3: Anomaly Detectors Rewrite (3-4 days)
**Goal**: Detectors read from NATS stream instead of ClickHouse queries

#### 3.1 HijackDetector Rewrite
- [ ] Create `src/anomaly_detection/nats_subscriber.rs`
  ```rust
  pub struct NatsSubscriber {
      client: async_nats::Client,
      consumer_group: String,
  }

  impl NatsSubscriber {
      pub async fn subscribe(url: &str) -> Result<tokio::sync::mpsc::Receiver<BgpEvent>> {
          let client = async_nats::connect(url).await?;
          // Create consumer group if not exists
          // Subscribe and return receiver channel
      }
  }
  ```

- [ ] Rewrite `src/anomaly_detection/hijack_detector.rs`
  - Remove: ClickHouse SELECT queries
  - Add: NATS subscriber channel
  - State: In-memory HashMap for 7-day baseline
    ```rust
    HashMap<Prefix, Vec<Asn>>  // Known ASN→Prefix mappings
    ```
  - Flow:
    1. Subscribe to nats://bgp-events
    2. For each event:
       - Check if (Prefix, ASN) pair is in baseline
       - If not: Anomaly detected
       - If 7 days passed: Freeze baseline
    3. Publish anomalies to Prometheus

- [ ] State Persistence
  - Option A: In-memory only (lose on restart)
  - Option B: Save to file (simple JSON)
  - Option C: Redis (complex but persistent)
  - **Recommendation**: Option B (file-based)

#### 3.2 FlappingDetector Rewrite
- [ ] Rewrite `src/anomaly_detection/flapping_detector.rs`
  - Remove: ClickHouse time-window queries
  - Add: In-memory sliding window (5 minutes)
  - State: Deque of (Prefix, Timestamp) tuples
    ```rust
    Deque<(Prefix, Timestamp)>  // Max 5 minute window
    ```
  - Flow:
    1. Subscribe to nats://bgp-events
    2. For each event:
       - Add (prefix, timestamp) to window
       - Remove entries older than 5 minutes
       - If count > threshold (e.g., 10): Anomaly
    3. Publish anomalies to Prometheus

#### 3.3 Database Cleanup
- [ ] Remove all ClickHouse references
- [ ] Remove clickhouse_exporter.rs
- [ ] Remove ClickHouse-dependent code from gateway.rs

#### 3.4 Testing
- [ ] Unit tests: Baseline calculation, flapping detection
- [ ] Integration test: NATS → Detector → Anomalies
- [ ] Load test: 14,600 events/sec detection latency

**Effort**: 3-4 days
**Risk**: State loss on restart (mitigation: file persistence)
**Deliverable**: Detectors consuming NATS, outputting anomalies with <100ms latency

---

### Phase 4: Prometheus Integration (1-2 days)
**Goal**: Anomaly detectors export metrics to Prometheus

#### 4.1 Metrics Exporter
- [ ] Create `src/metrics_exporter.rs`
  ```rust
  pub struct MetricsExporter {
      metrics: HashMap<String, Counter>,
      http_server: tokio::net::TcpListener,
  }

  impl MetricsExporter {
      pub fn new(listen_addr: &str) -> Result<Self> {
          // Initialize Prometheus metrics:
          // - anomalies_total{type="hijack"}
          // - anomalies_total{type="flapping"}
          // - detection_latency_seconds
          // - stream_events_processed_total
      }

      pub async fn serve() {
          // Serve /metrics endpoint for Prometheus scraping
      }
  }
  ```

- [ ] Define metrics
  - `hijack_anomalies_detected_total` (Counter)
  - `flapping_anomalies_detected_total` (Counter)
  - `anomaly_detection_latency_seconds` (Histogram)
  - `bgp_events_processed_total` (Counter)
  - `baseline_training_progress` (Gauge: 0-100)
  - `nats_subscription_lag_seconds` (Gauge)

- [ ] Update Prometheus config
  - `deploy/prometheus.prod.yml`
    ```yaml
    scrape_configs:
      - job_name: 'hijack-detector'
        static_configs:
          - targets: ['localhost:9091']  # New metrics port
    ```

#### 4.2 Testing
- [ ] Metrics endpoint: `curl http://localhost:9091/metrics`
- [ ] Prometheus scraping: verify metrics in Prometheus UI

**Effort**: 1-2 days
**Risk**: Metric cardinality explosion (if not careful with labels)
**Deliverable**: Prometheus scraping anomaly detector metrics

---

### Phase 5: Grafana Dashboard Update (1-2 days)
**Goal**: Grafana queries Prometheus instead of ClickHouse

#### 5.1 Remove ClickHouse Datasource
- [ ] Delete: ClickHouse datasource from Grafana
- [ ] Keep: Prometheus datasource

#### 5.2 Rewrite Dashboards
- [ ] Update `deploy/grafana/provisioning/dashboards/gateway.json`
  - Old: SQL queries against ClickHouse
  - New: PromQL queries against Prometheus
  - Panels:
    ```
    - Anomalies/hour (rate(anomalies_total[1h]))
    - Anomaly types (rate by type label)
    - Detection latency (histogram_quantile)
    - Events/sec (rate(events_processed_total[1m]))
    - BGP flaps/5min
    - Training progress (baseline gauge)
    ```

#### 5.3 Testing
- [ ] Login to Grafana (http://server:3001)
- [ ] Verify all queries execute without error
- [ ] Visual check: graphs display data

**Effort**: 1-2 days
**Risk**: Missing PromQL knowledge → queries don't work
**Deliverable**: Grafana dashboards working with Prometheus

---

### Phase 6: Docker Compose Integration (1 day)
**Goal**: Update docker-compose.prod.yml with all changes

#### 6.1 Service Updates
- [ ] Add NATS service
- [ ] Modify bgp-stream service (NATS_URL env, remove GATEWAY_URL)
- [ ] Modify gateway service (optional: health-check only mode)
- [ ] Remove: clickhouse depends_on from gateways
- [ ] Modify: haproxy config (remove /logs endpoint, add /health)

#### 6.2 Network & Health Checks
- [ ] NATS health check
- [ ] Detector health checks (via metrics endpoint)
- [ ] Remove ClickHouse health check (if not needed for other services)

#### 6.3 Volumes
- [ ] Remove: clickhouse-data, clickhouse-logs volumes
- [ ] Add: nats-data volume (if persistent storage needed)
- [ ] Keep: all other volumes

**Effort**: 1 day
**Risk**: Service startup order issues (NATS before bgp-stream)
**Deliverable**: docker-compose.prod.yml running all services

---

### Phase 7: Testing & Validation (1-2 days)
**Goal**: Full system integration test

#### 7.1 End-to-End Test
- [ ] Start docker-compose
- [ ] Verify all services healthy
- [ ] Start bgp-stream
- [ ] Check NATS messages arriving (nats sub bgp.events)
- [ ] Check detectors running
- [ ] Check Prometheus metrics collecting
- [ ] Check Grafana dashboards displaying data
- [ ] 1-hour stability test (no crashes)

#### 7.2 Performance Benchmarks
- [ ] BGP event throughput: 14,600 events/sec
- [ ] Anomaly detection latency: <100ms
- [ ] NATS buffer usage: <50MB
- [ ] Prometheus disk: <1GB (15 days)
- [ ] CPU usage: baseline vs ClickHouse comparison

#### 7.3 Data Integrity
- [ ] No lost events (count in NATS = count processed)
- [ ] Baseline correctness (7-day window)
- [ ] Anomalies match old system (validation period)

**Effort**: 1-2 days
**Risk**: Data validation complexity
**Deliverable**: Pass all integration tests, performance targets met

---

### Phase 8: Documentation & Cleanup (0.5 days)
**Goal**: Documentation, monitoring, and ClickHouse removal

#### 8.1 Documentation
- [ ] Update README.md with new architecture
- [ ] Add NATS configuration guide
- [ ] Add Prometheus/PromQL query examples
- [ ] Update troubleshooting guide

#### 8.2 Monitoring Setup
- [ ] Add Prometheus alert rules for streaming
  - NATS consumer lag
  - Detector processing latency
  - Stream event count anomalies

#### 8.3 Optional: Remove ClickHouse
- [ ] Delete ClickHouse container (or keep for fallback)
- [ ] Remove init.sql schema files
- [ ] Remove ClickHouse volume mount
- [ ] Deploy to production

**Effort**: 0.5 days
**Risk**: If keeping ClickHouse: extra complexity
**Deliverable**: Production-ready streaming system

---

## 5. Timeline & Milestones

```
Week 1 (March 5-12):
├─ Day 1-2:  Phase 1 (NATS setup)
├─ Day 3-4:  Phase 2 (BGP-Stream rewrite)
├─ Day 5:    Phase 3 Start (HijackDetector)
└─ Day 6-7:  Phase 3 Continue (FlappingDetector)

Week 2 (March 12-19):
├─ Day 8:    Phase 3 Finish + Testing
├─ Day 9:    Phase 4 (Prometheus integration)
├─ Day 10:   Phase 5 (Grafana dashboards)
├─ Day 11:   Phase 6 (Docker-compose integration)
├─ Day 12:   Phase 7 (Full integration testing)
└─ Day 13:   Phase 8 (Documentation & cleanup)

TOTAL: ~10 working days

Milestones:
🟢 March 7 (Day 2):  NATS running, healthy
🟢 March 9 (Day 4):  bgp-stream → NATS verified
🟢 March 12 (Day 7): Detectors reading from NATS
🟢 March 14 (Day 9): Prometheus collecting metrics
🟢 March 16 (Day 11): Full system integration test PASS
🟢 March 19 (Day 14): Production deployment
```

---

## 6. Detailed Change Matrix

### Rust Code Changes

| File | Status | Changes | Lines | Effort |
|------|--------|---------|-------|--------|
| tools/bgp_stream/src/main.rs | REWRITE | Remove gateway client, add NATS publisher | ±100 | 2d |
| tools/bgp_stream/src/nats_publisher.rs | NEW | NATS publishing logic | ~150 | 1d |
| tools/bgp_stream/src/gateway_client.rs | DELETE | No longer needed | - | - |
| src/anomaly_detection/hijack_detector.rs | REWRITE | NATS subscriber, in-memory baseline | ±200 | 2d |
| src/anomaly_detection/flapping_detector.rs | REWRITE | NATS subscriber, sliding window | ±150 | 1d |
| src/anomaly_detection/nats_subscriber.rs | NEW | NATS client wrapper | ~100 | 1d |
| src/metrics_exporter.rs | NEW | Prometheus metrics endpoint | ~200 | 1d |
| src/clickhouse_exporter.rs | DELETE | No longer needed | - | - |
| src/main.rs (gateway) | MODIFY | Remove ClickHouse init, add metrics | ±50 | 0.5d |
| Cargo.toml | MODIFY | Add async-nats, prometheus, remove clickhouse deps | ±20 | - |

### Configuration Changes

| File | Status | Changes | Effort |
|------|--------|---------|--------|
| docker-compose.prod.yml | MODIFY | Add NATS, update env vars, remove ClickHouse refs | 1d |
| deploy/nats/nats.conf | NEW | NATS Jetstream configuration | 0.5d |
| deploy/prometheus.prod.yml | MODIFY | Add detector scrape job | 0.5d |
| deploy/haproxy/haproxy.cfg | MODIFY | Remove /logs endpoint (optional) | 0.5d |
| deploy/grafana/provisioning/dashboards/ | MODIFY | Update all queries to PromQL | 1d |

### Deletions

| Component | Impact |
|-----------|--------|
| ClickHouse Service | -4.1GB storage, -60GB layer cache |
| clickhouse_exporter.rs | Simplify codebase |
| deploy/clickhouse/init.sql | Simplify deployment |
| HAProxy logs endpoint | Simplify routing |

---

## 7. Risk Assessment & Mitigation

| Risk | Severity | Impact | Mitigation |
|------|----------|--------|-----------|
| **NATS message loss** | High | Missed anomalies | Enable persistence, ack acknowledgments |
| **State loss on restart** | Medium | Inaccurate baseline | File-based persistence for baseline |
| **Detector latency increase** | Medium | Slower anomaly detection | In-memory indexes, optimize query paths |
| **Prometheus cardinality** | Medium | Query slowdown | Carefully design labels, avoid unbounded dimensions |
| **ClickHouse data loss** | Low | Lose historical data | Archive to S3 before deletion |
| **Deployment failure** | Medium | Service downtime | Blue-green deployment, keep ClickHouse as fallback |
| **PromQL query complexity** | Low | Dashboard issues | Start with simple queries, iterate |
| **ARM64 compatibility** | Low | Binary incompatibility | Test on target hardware early |

### Contingency Plans
1. **If NATS unstable**: Fallback to Redis Streams (drop-in replacement)
2. **If state loss critical**: Keep ClickHouse alongside (hybrid mode for 1-2 weeks)
3. **If latency issues**: Add local caching layer (fast memory + NATS subscription)
4. **If PromQL learning curve**: Use pre-built Grafana templates, customize gradually

---

## 8. Success Criteria

### Functional Requirements
- [ ] BGP events flow from RIPE RIS → NATS → Detectors without loss
- [ ] Anomalies detected in <100ms after event arrival
- [ ] Baseline trained within 7 days, frozen correctly
- [ ] Prometheus metrics accurate and queryable
- [ ] Grafana dashboards display all monitoring data
- [ ] All alerts trigger correctly
- [ ] System stable for 24h+ without crashes

### Performance Requirements
- [ ] Handle 14,600 events/sec continuously
- [ ] NATS buffer stays <50MB
- [ ] Detection latency p99 <150ms
- [ ] CPU usage <4 cores (per detector)
- [ ] Memory usage <1GB (detectors + NATS)

### Storage Requirements
- [ ] Daily storage: <5GB (vs 24GB currently)
- [ ] NATS retention: 24h
- [ ] Prometheus retention: 15 days
- [ ] Total storage used: <100GB (vs 75GB+ full)

### Operational Requirements
- [ ] Deployment via docker-compose (no manual steps)
- [ ] Health checks for all services
- [ ] Logs aggregated and searchable
- [ ] Metrics accessible via Prometheus
- [ ] Alerting functional via AlertManager

---

## 9. Post-Implementation Optimizations (Future)

### Optional Phase 9: Archive Service (2-3 days)
- Implement daily batch archival to S3/MinIO
- Compress BGP events with zstd (75% reduction)
- Enable 90-day historical analysis
- Estimated cost: €30/month for S3

### Optional Phase 10: Baseline Freezing (1-2 days)
- Save trained baseline as JSON after 7 days
- Delete raw events, keep only baseline + anomalies
- Reduce storage from 7GB/day → 1GB/day
- Better for compliance and performance

### Optional Phase 11: Horizontal Scaling (3-5 days)
- Partition NATS subjects by ASN ranges
- Deploy detector instances per partition
- Add NATS cluster for redundancy
- Enable multi-region deployment

---

## 10. Rollback Plan

### Scenario 1: Streaming system instability (Day 1-5)
```
Action: Keep ClickHouse running in parallel
├─ Both bgp-stream → NATS and → ClickHouse
├─ Detectors read from NATS (primary)
├─ ClickHouse as fallback data source
├─ Once confident, delete ClickHouse
```

### Scenario 2: Critical production issue (Day 6+)
```
Action: Switch to ClickHouse, redeploy streaming later
├─ Stop detectors (or keep them for redundancy)
├─ Restore ClickHouse volume from backup
├─ Revert docker-compose.prod.yml
├─ bgp-stream → ClickHouse (old config)
└─ Fix issues, retry streaming phase
```

### Scenario 3: Data validation failure
```
Action: Keep both systems for validation period
├─ Run streaming system (NATS → detectors → metrics)
├─ Query ClickHouse in parallel for historical validation
├─ Compare anomaly counts, latency, accuracy
├─ Once validated, remove ClickHouse
└─ Fix any discrepancies in streaming logic
```

---

## 11. Team Responsibilities

| Role | Responsibility | Duration |
|------|-----------------|----------|
| **Backend Dev** | Rust code changes (Phases 2-4) | 6-7 days |
| **DevOps** | Docker/infra changes (Phase 6) | 1-2 days |
| **QA** | Testing & validation (Phase 7) | 1-2 days |
| **Tech Lead** | Architecture review, decisions | Throughout |
| **Tech Lead** | Documentation (Phase 8) | 0.5 days |

---

## 12. Communication Plan

### Daily Standup
- 10:00 AM: 15min sync on Phase progress
- Blocker identification
- Resource requests

### Weekly Review (Every Friday)
- Milestone status
- Risk assessment update
- Stakeholder communication

### Deployment Day Coordination
- Pre-deployment checklist
- Rollback procedures review
- On-call engineer assigned

---

## 13. Appendix: Configuration Templates

### NATS Configuration
```nginx
# deploy/nats/nats.conf
port: 4222
max_payload: 16MB

jetstream {
  store_dir: /data/jetstream
  max_mem_store: 1G
  max_file_store: 10G
}

accounts {
  SYS: {
    users: [
      { user: sys, password: sys }
    ]
  }
  BGP: {
    users: [
      { user: bgp-stream, password: ${NATS_PASSWORD} }
    ]
    jetstream: enabled
  }
}

system_account: SYS
```

### Prometheus Scrape Config
```yaml
# deploy/prometheus.prod.yml additions
scrape_configs:
  - job_name: 'hijack-detector'
    static_configs:
      - targets: ['127.0.0.1:9091']
    scrape_interval: 30s

  - job_name: 'flapping-detector'
    static_configs:
      - targets: ['127.0.0.1:9092']
    scrape_interval: 30s
```

### PromQL Query Examples
```promql
# Anomalies per hour
rate(anomalies_total[1h])

# Detection latency (p99)
histogram_quantile(0.99, detection_latency_seconds_bucket)

# Events processed per second
rate(events_processed_total[1m])

# Baseline training progress
baseline_training_progress_percent
```

---

## 14. Sign-Off

| Role | Name | Date | Signature |
|------|------|------|-----------|
| Tech Lead | - | 2026-03-05 | Approved ✓ |
| DevOps Lead | - | 2026-03-05 | Approved ✓ |
| Product Owner | - | 2026-03-05 | Approved ✓ |

---

**Document Version**: 1.0
**Last Updated**: 2026-03-05
**Next Review**: 2026-03-19 (Post-deployment)
