# Migration Guide: ClickHouse → NATS Streaming

## Overview

This guide covers the planned migration from ClickHouse-only storage to NATS Jetstream-based streaming architecture while maintaining operational continuity.

## Architecture Comparison

### Before (ClickHouse-Only)
```
BGP-Stream → HTTP POST → Gateway → ClickHouse
                            ↓
                        Anomaly Detector
                            ↓
                        ClickHouse (24GB/day)
```

**Problems**:
- ❌ 24GB/day storage growth (56GB disk fills in 2 days)
- ❌ Slow anomaly detection (post-processing on ClickHouse)
- ❌ Disk I/O bottleneck at 14,600 events/sec
- ❌ Limited historical learning (7-day warmup needed)

### After (NATS Streaming)
```
BGP-Stream → NATS Jetstream → Detector → Prometheus → Grafana
                ↓
            (1GB in-memory)
            (10GB file-store)
```

**Benefits**:
- ✅ ~87% storage reduction (3GB/day vs 24GB/day)
- ✅ Real-time anomaly detection
- ✅ Publish-subscribe pattern (scalable)
- ✅ In-memory detection (< 10ms latency)

## Migration Phases

### Phase 0: Pre-Migration (Current - Do Now)

**Duration**: 1 day before cutover

```bash
# 1. Verify current ClickHouse storage
du -h /root/log-gateway/data

# 2. Export summary of known hijacks/flappings
curl -X GET 'http://localhost:8123' \
  -d "SELECT prefix, origin_as, COUNT() as count FROM bgp.anomalies GROUP BY prefix, origin_as"

# 3. Backup ClickHouse data
docker exec log_clickhouse \
  clickhouse-client --query "BACKUP TABLE bgp TO 'file:///backups/pre-migration'"

# 4. Document current performance baseline
curl 'http://localhost:9090/api/v1/query?query=up' | jq '.data.result'
```

### Phase 1: Parallel Operation (Day 1-2)

**Goal**: Validate NATS pipeline while keeping ClickHouse active

**Config**:
```toml
# config/default.toml
[nats]
enabled = true              # Enable streaming
url = "nats://nats:4222"

[clickhouse]
enabled = true              # Keep ClickHouse active
url = "http://clickhouse:8123"
```

**Verification**:
```bash
# Both systems receiving data
curl 'http://localhost:9090/api/v1/query?query=detector_events_processed_total'  # NATS
curl 'http://localhost:9090/api/v1/query?query=clickhouse_ingest_total'  # ClickHouse

# Both should show increasing counts
```

### Phase 2: Validation (Day 2-3)

**Duration**: 24-48 hours

**Tests**:
1. ✅ NATS events flowing: 14,600 events/sec
2. ✅ Detectors working: anomalies detected
3. ✅ Metrics exported: Prometheus updated
4. ✅ Grafana dashboard: showing data
5. ✅ Alerts firing: AlertManager working
6. ✅ No errors: error rate < 0.1%

**Abort Criteria** (Rollback to ClickHouse-only):
- ❌ Processing rate drops below 10,000 events/sec
- ❌ Error rate exceeds 1%
- ❌ Memory grows > 50% without stabilizing
- ❌ Prometheus scrape failures

### Phase 3: Switchover (Day 3)

**Duration**: ~30 minutes downtime

**Steps**:
1. Stop BGP-Stream
2. Verify all events processed
3. Disable ClickHouse in config
4. Restart gateways
5. Resume BGP-Stream
6. Verify metrics normal

**Detailed Procedure**:

```bash
# 1. Mark migration window (notify ops team)
echo "Starting NATS migration at $(date)" | tee /tmp/migration.log

# 2. Pause BGP-Stream (waits for last batch)
docker-compose pause bgp-stream
sleep 30

# 3. Verify all events processed
PROCESSED=$(curl -s 'http://localhost:9090/api/v1/query?query=detector_events_processed_total' | jq '.data.result[0].value[1]')
echo "Events processed: $PROCESSED" | tee -a /tmp/migration.log

# 4. Disable ClickHouse
sed -i 's/enabled = true  # ClickHouse/enabled = false  # Disabled in Phase 3/' \
  /root/log-gateway/config/default.toml

# 5. Restart gateways (pick one at a time for zero-downtime if load-balanced)
docker-compose restart gateway_1
sleep 10
docker-compose restart gateway_2
sleep 10
docker-compose restart gateway_3
sleep 10
docker-compose restart gateway_4

# 6. Resume BGP-Stream
docker-compose unpause bgp-stream
sleep 5

# 7. Verify normal operation
curl 'http://localhost:9090/api/v1/query?query=detector_events_processed_total' | jq '.data.result'

echo "Migration complete at $(date)" | tee -a /tmp/migration.log
```

### Phase 4: Monitoring (Day 4-7)

**Duration**: 7 days post-migration

**Daily Checks**:
```bash
#!/bin/bash
# daily-health-check.sh

# 1. Processing metrics
RATE=$(curl -s 'http://localhost:9090/api/v1/query?query=rate(detector_events_processed_total[5m])' \
  | jq '.data.result[0].value[1]')
echo "Processing rate: $RATE events/sec"

# 2. Anomaly detection
ANOMALIES=$(curl -s 'http://localhost:9090/api/v1/query?query=detector_anomalies_detected_total' \
  | jq '.data.result[].value[1]' | jq -s 'add')
echo "Anomalies detected: $ANOMALIES"

# 3. Error rate
ERROR_RATE=$(curl -s 'http://localhost:9090/api/v1/query?query=rate(detector_processing_errors_total[5m])' \
  | jq '.data.result[0].value[1]')
echo "Error rate: $ERROR_RATE errors/sec"

# 4. System health
docker stats --no-stream gateway_1 | tail -1 | awk '{print "Gateway memory: " $4}'

# 5. Alert status
curl -s http://localhost:9093/api/v1/alerts | jq '.data | length' | xargs -I {} echo "Active alerts: {}"
```

## Fallback Procedures

### Quick Rollback (Hotfix within 1 hour)

```bash
# 1. Disable NATS
sed -i 's/enabled = true  # NATS/enabled = false  # Rollback/' \
  /root/log-gateway/config/default.toml

# 2. Enable ClickHouse
sed -i 's/enabled = false  # ClickHouse/enabled = true/' \
  /root/log-gateway/config/default.toml

# 3. Restart gateways
docker-compose restart gateway_1 gateway_2 gateway_3 gateway_4

# 4. Verify ClickHouse receiving data
curl 'http://localhost:9090/api/v1/query?query=clickhouse_ingest_total'
```

### Safe Rollback (with data preservation)

```bash
# 1. Enable hybrid mode (NATS + ClickHouse)
# config/default.toml
[nats]
enabled = true              # Keep NATS for analysis

[clickhouse]
enabled = true              # Re-enable for backup

# 2. Gradually migrate readers to Grafana/Prometheus
# (NATS provides real-time, ClickHouse for historical)

# 3. After 1-2 weeks, disable ClickHouse
sed -i 's/enabled = true  # ClickHouse/enabled = false/' \
  /root/log-gateway/config/default.toml
```

## Risk Mitigation

### Backup Strategy

**Before Migration**:
```bash
# Full ClickHouse backup
docker exec log_clickhouse \
  clickhouse-client --query "BACKUP TABLE bgp TO 'file:///var/lib/clickhouse/backups/pre-migration'"

# Verify backup
docker exec log_clickhouse \
  ls -lh /var/lib/clickhouse/backups/
```

**During Migration**:
```bash
# Keep ClickHouse container running (but disabled)
# Do NOT delete ClickHouse volume

# Monitor hybrid mode
# Both systems active = instant rollback capability
```

**After Migration (1 week)**:
```bash
# Verify NATS stability for 1 week
# Then archive ClickHouse backup
docker exec log_clickhouse \
  tar czf /backups/clickhouse-pre-migration.tar.gz /var/lib/clickhouse/data
```

### Testing Checklist

- ✅ Load test: 14,600 events/sec for 1 hour
- ✅ Anomaly accuracy: Compare detector output with ClickHouse baseline
- ✅ Latency: Event → Grafana < 500ms
- ✅ Storage: ~3GB/day vs 24GB/day baseline
- ✅ Memory stability: < 10% growth over 1 week
- ✅ Alert accuracy: All alert rules firing correctly
- ✅ Grafana dashboard: All panels working
- ✅ Error handling: Graceful degradation on NATS failure

## Success Criteria

**Migration is successful when**:

1. **Functionality**
   - ✅ All BGP events processed in real-time
   - ✅ Anomalies detected and exported as metrics
   - ✅ Grafana dashboard fully operational
   - ✅ Alerts firing on thresholds

2. **Performance**
   - ✅ Processing rate: 14,600 events/sec sustained
   - ✅ Latency: < 500ms event → metrics
   - ✅ Error rate: < 0.1%
   - ✅ Memory stable: < 2GB per gateway

3. **Reliability**
   - ✅ Zero data loss (all events processed)
   - ✅ Graceful restart (no event backlog)
   - ✅ Connection resilience (auto-reconnect)
   - ✅ Metric consistency (no gaps)

4. **Cost**
   - ✅ Storage: 87% reduction (3GB/day vs 24GB/day)
   - ✅ Network: No increase
   - ✅ CPU: Within limits
   - ✅ Memory: Acceptable per instance

## Post-Migration Tasks

### Week 1-2: Stabilization

- ✅ Daily health checks (see monitoring section)
- ✅ Alert testing (verify all rules work)
- ✅ Performance analysis (measure real-world stats)
- ✅ Documentation review (update runbooks)

### Week 2-4: Optimization

- Fine-tune scrape intervals (Prometheus)
- Optimize recording rules
- Profile detector performance
- Document best practices

### Month 1+: Maintenance

- Archive ClickHouse backup if stable
- Update monitoring dashboards
- Create migration playbook for future reference
- Train team on new architecture

## Communication Plan

### Before Migration

**Email to team**:
```
Subject: NATS Streaming Migration - March 6, 2026

Hi team,

We're migrating from ClickHouse to NATS Jetstream streaming on March 6.

Timeline:
- Phase 1-2: Parallel validation (March 1-3)
- Phase 3: Switchover (March 3, 3-4 AM UTC)
- Phase 4: Monitoring (March 4-10)

Impact:
- 30-minute downtime during switchover
- After: 87% storage reduction, real-time anomaly detection
- Rollback available if issues detected

Questions? See docs/MIGRATION_FROM_CLICKHOUSE.md
```

### During Migration

**Slack updates**:
```
11:55 PM - Starting NATS migration
12:00 AM - Pausing BGP-Stream
12:05 AM - Verifying event processing
12:10 AM - Switching configuration
12:20 AM - Restarting gateways
12:30 AM - Resuming BGP-Stream
12:35 AM - Verifying normal operation
12:40 AM - Migration complete ✅
```

### Post-Migration

**Daily summary**:
- Processing rate: OK
- Error rate: OK
- Anomalies detected: OK
- No issues

---

**Migration Owner**: DevOps Team
**Approval Required**: Engineering Lead
**Estimated Downtime**: 30 minutes
**Rollback Time**: < 5 minutes

---

**Reference**: /root/log-gateway/STREAMING_ROADMAP.md
**Last Updated**: 2026-03-06
