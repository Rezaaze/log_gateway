# Phase 0: Pre-Migration Deployment Package

> **⚠️ VERALTET (Stand 08.09.2026).** Dieses Dokument beschreibt Arbeiten an
> einem Server, der stillgelegt wurde und nicht mehr existiert
> (167.235.30.106). Die beschriebenen Schritte nicht mehr ausführen. Das
> Dokument bleibt als Protokoll erhalten; den aktuellen Stand beschreibt
> `CLAUDE.md`.

**Status**: READY FOR DEPLOYMENT 🚀
**Date**: 2026-03-06
**Deployment Window**: 24 hours before cutover
**Duration**: ~1-2 hours

---

## 📋 Pre-Deployment Checklist (Local)

### ✅ Code & Configuration Ready
- [x] Phase 1-6 implementation complete
- [x] All unit tests passing (205 tests)
- [x] Code compiled without errors
- [x] Docker images built (`log-gateway`, `bgp-stream`)
- [x] NATS configuration: `deploy/nats/nats.conf`
- [x] Prometheus config: `deploy/prometheus.prod.yml`
- [x] AlertManager config: `deploy/alertmanager/detector-alerts.yml`
- [x] Grafana dashboard: `deploy/grafana/provisioning/dashboards/bgp-detector-dashboard.json`
- [x] docker-compose.prod.yml includes all services (NATS, Prometheus, Grafana, AlertManager)

### ✅ Documentation Complete
- [x] Migration guide: `docs/MIGRATION_FROM_CLICKHOUSE.md`
- [x] Testing procedures: `docs/END_TO_END_TESTING.md`
- [x] Troubleshooting guide: `docs/TROUBLESHOOTING.md`
- [x] Prometheus metrics: `docs/PROMETHEUS_METRICS.md`

---

## 🚀 Deployment Steps

### Step 1: Verify Local Build

```bash
# Build Docker images
cd /Users/alirezashahsavarkhani/rust_tool/log-gateway
docker build -f Dockerfile -t log-gateway:latest .
docker build -f tools/bgp_stream/Dockerfile -t bgp-stream:latest tools/bgp_stream

# Verify images
docker images | grep -E "log-gateway|bgp-stream"
```

**Expected Output**:
```
log-gateway              latest    <image-id>    <date>    <size>
bgp-stream               latest    <image-id>    <date>    <size>
```

---

### Step 2: Prepare Production Server

**On Server** (`root@167.235.30.106`):

```bash
# 1. SSH into production server
ssh root@167.235.30.106

# 2. Create backup directory
mkdir -p /root/log-gateway-backup/$(date +%Y%m%d)

# 3. Backup current ClickHouse data
echo "=== Backing up ClickHouse data ==="
docker exec clickhouse clickhouse-client --query "BACKUP TABLE bgp TO 'file:///backups/pre-phase1'"

# 4. Verify backup
docker exec clickhouse ls -lh /var/lib/clickhouse/backups/

# 5. Document current metrics baseline
echo "=== Documenting baseline metrics ==="
curl -s 'http://localhost:9090/api/v1/query?query=up' | jq . > /tmp/baseline_metrics.json
curl -s 'http://localhost:9090/api/v1/query?query=detector_events_processed_total' | jq . >> /tmp/baseline_metrics.json

# 6. Current disk usage
du -sh /root/log-gateway/data
du -sh /root/log-gateway/clickhouse-data
```

---

### Step 3: Verify NATS Configuration

**On Server**:

```bash
# 1. Check NATS service is healthy
docker-compose -f docker-compose.prod.yml ps nats

# 2. Verify NATS is listening
nc -zv localhost 4222

# 3. Test NATS HTTP API
curl -s http://localhost:8222/healthz | jq .

# 4. Expected output: {"ok": true}
```

---

### Step 4: Verify Prometheus & Grafana

**On Server**:

```bash
# 1. Check Prometheus is scraping targets
curl -s http://localhost:9090/api/v1/targets | jq '.data.activeTargets | length'

# Expected: Shows number of active scrape targets (should match gateway instances)

# 2. Verify Grafana datasource
curl -s http://localhost:3001/api/datasources | jq '.[] | .name'

# Expected: "Prometheus" and other datasources

# 3. Check dashboard is loaded
curl -s http://localhost:3001/api/dashboards/uid/bgp-detector | jq '.dashboard.title'

# Expected: "BGP Anomaly Detection Pipeline"
```

---

### Step 5: Prepare for Phase 1 (Parallel Operation)

Ensure the following before starting Phase 1:

**Gateway Configuration** (`/root/log-gateway/config/default.toml`):

```toml
# NATS configuration (should be enabled)
[nats]
enabled = true
url = "nats://nats:4222"

# ClickHouse configuration (keep enabled for parallel operation)
[clickhouse]
enabled = true
url = "http://clickhouse:8123"
```

**Verification** (on server):

```bash
# 1. Check current state of services
docker-compose -f docker-compose.prod.yml ps

# 2. Expected: All containers should be running
# NAME                COMMAND                  SERVICE         STATUS
# gateway_1           "/app/log-gateway"      gateway_1       Up (healthy)
# gateway_2           "/app/log-gateway"      gateway_2       Up (healthy)
# gateway_3           "/app/log-gateway"      gateway_3       Up (healthy)
# gateway_4           "/app/log-gateway"      gateway_4       Up (healthy)
# haproxy-lb          "/docker-entrypoi..."   haproxy         Up (healthy)
# nats                "server ..."            nats            Up (healthy)
# prometheus          "/bin/prometheus ..."   prometheus      Up
# grafana             "/run.sh"               grafana         Up
# alertmanager        "/bin/alertmanager"     alertmanager    Up
# clickhouse          "entrypoint.sh click..."  clickhouse    Up (healthy)
```

---

## 📊 System State Snapshot

Before Phase 1 begins, run this to document baseline:

```bash
#!/bin/bash
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_DIR="/tmp/pre-migration-snapshot-$TIMESTAMP"
mkdir -p "$BACKUP_DIR"

echo "=== Phase 0: Pre-Migration Snapshot ===" | tee "$BACKUP_DIR/snapshot.log"
echo "Timestamp: $(date)" | tee -a "$BACKUP_DIR/snapshot.log"

# 1. ClickHouse status
echo -e "\n=== ClickHouse Status ===" | tee -a "$BACKUP_DIR/snapshot.log"
docker exec clickhouse clickhouse-client --query "SELECT COUNT() as total_events FROM bgp.bgp_stream;" | tee -a "$BACKUP_DIR/snapshot.log"

# 2. Current metrics
echo -e "\n=== Prometheus Metrics ===" | tee -a "$BACKUP_DIR/snapshot.log"
curl -s 'http://localhost:9090/api/v1/query?query=detector_events_processed_total' | jq . | tee "$BACKUP_DIR/metrics.json"

# 3. Disk usage
echo -e "\n=== Disk Usage ===" | tee -a "$BACKUP_DIR/snapshot.log"
du -sh /root/log-gateway/data | tee -a "$BACKUP_DIR/snapshot.log"
du -sh /root/log-gateway/clickhouse-data | tee -a "$BACKUP_DIR/snapshot.log"

# 4. Docker stats
echo -e "\n=== Docker Container Stats ===" | tee -a "$BACKUP_DIR/snapshot.log"
docker stats --no-stream | tee -a "$BACKUP_DIR/snapshot.log"

echo -e "\n✅ Snapshot saved to: $BACKUP_DIR"
```

---

## ⚠️ Abort Criteria

If any of these conditions are met, **STOP** and troubleshoot before proceeding:

- ❌ NATS health check fails
- ❌ Prometheus not scraping targets
- ❌ Grafana dashboard not accessible
- ❌ ClickHouse backup fails
- ❌ Disk space < 20GB remaining
- ❌ Any container failing to start

---

## 🔄 Phase 1 Readiness Check

After Phase 0 completes, verify:

```bash
# 1. All services healthy
docker-compose -f docker-compose.prod.yml ps | grep -E "Up|running"

# 2. NATS accepting connections
timeout 2 bash -c 'echo ping | nc -w 1 localhost 4222' && echo "✅ NATS OK" || echo "❌ NATS FAIL"

# 3. ClickHouse accepting queries
docker exec clickhouse clickhouse-client --query "SELECT 1" && echo "✅ ClickHouse OK" || echo "❌ ClickHouse FAIL"

# 4. Prometheus scraping
curl -s http://localhost:9090/api/v1/targets | jq '.data.activeTargets | length' && echo "✅ Prometheus OK"

# 5. Grafana accessible
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000 && echo "✅ Grafana OK"
```

**Expected**: All checks return OK ✅

---

## 📝 Next Steps

After Phase 0 completes successfully:

1. **Proceed to Phase 1**: Parallel Operation (Day 1-2)
   - Both NATS and ClickHouse active
   - Validation of NATS pipeline
   - No user-facing changes yet

2. **Documentation**: Follow `docs/MIGRATION_FROM_CLICKHOUSE.md`

3. **Timeline**:
   - Phase 1: 24-48 hours (parallel validation)
   - Phase 2: 24-48 hours (testing)
   - Phase 3: ~30 minutes (switchover)
   - Phase 4: 7 days (monitoring)

---

## 🆘 Support

If any issues occur:

1. Check logs: `docker compose -f docker-compose.prod.yml logs -f <service>`
2. Consult: `docs/TROUBLESHOOTING.md`
3. Rollback: Quick rollback available (see `docs/MIGRATION_FROM_CLICKHOUSE.md`)

---

**Status**: Phase 0 checklist ready for execution
**Approval**: Ready for Phase 1 after all checks pass ✅
