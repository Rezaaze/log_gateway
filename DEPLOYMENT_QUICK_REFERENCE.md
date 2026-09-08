# 🚀 Deployment Quick Reference

> **⚠️ VERALTET (Stand 08.09.2026).** Dieses Dokument beschreibt Arbeiten an
> einem Server, der stillgelegt wurde und nicht mehr existiert
> (167.235.30.106). Die beschriebenen Schritte nicht mehr ausführen. Das
> Dokument bleibt als Protokoll erhalten; den aktuellen Stand beschreibt
> `CLAUDE.md`.

**NATS Streaming Migration - Ready for Deployment**

---

## 📍 Start Here

**New to this deployment?** Start with these files in order:

1. **DEPLOYMENT_STATUS.md** ← START HERE
   - Overview of entire implementation
   - Timeline and quick start
   - Success metrics and checklist

2. **PHASE_0_DEPLOYMENT.md**
   - Pre-migration verification procedures
   - System state snapshot collection
   - Pre-flight checklist

3. **docs/MIGRATION_FROM_CLICKHOUSE.md**
   - Complete 4-phase migration plan
   - Detailed procedures for each phase
   - Rollback instructions

---

## 🚀 Deployment in 4 Steps

### Step 1: Pre-Migration (Phase 0) - ~1-2 hours
```bash
# On production server (root@167.235.30.106)
cd /root/log-gateway
./deploy/phase-0-pre-migration.sh
```
**Result**: Backup complete, baseline metrics documented ✅

### Step 2: Parallel Operation (Phase 1) - 24-48 hours
```bash
# On production server
./deploy/phase-1-parallel-operation.sh start
./deploy/phase-1-parallel-operation.sh verify
```
**Result**: Both NATS and ClickHouse running, validating ✅

### Step 3: Validation (Phase 2) - 24-48 hours
```bash
# Follow docs/END_TO_END_TESTING.md
# Run integration tests, load tests, alert verification
```
**Result**: All tests passing ✅

### Step 4: Switchover (Phase 3) - ~30 minutes
```bash
# Follow docs/MIGRATION_FROM_CLICKHOUSE.md Phase 3
# Automated switchover with zero-downtime gateway restarts
```
**Result**: ClickHouse disabled, NATS-only mode active ✅

---

## 📚 Documentation Map

| Document | Purpose | Read Time |
|----------|---------|-----------|
| **DEPLOYMENT_STATUS.md** | Overview & timeline | 10 min |
| **PHASE_0_DEPLOYMENT.md** | Pre-migration checklist | 15 min |
| **docs/MIGRATION_FROM_CLICKHOUSE.md** | 4-phase migration plan | 20 min |
| **docs/END_TO_END_TESTING.md** | Integration testing procedures | 15 min |
| **docs/TROUBLESHOOTING.md** | Issue resolution (8 guides) | 20 min |
| **docs/PROMETHEUS_METRICS.md** | Metrics reference | 10 min |
| **docs/GRAFANA_DASHBOARDS.md** | Dashboard details | 10 min |

---

## 🔧 Deployment Scripts

### Phase 0: Pre-Migration
**File**: `deploy/phase-0-pre-migration.sh`
**What it does**:
- ✅ Verifies all services running
- ✅ Backs up ClickHouse data
- ✅ Documents baseline metrics
- ✅ Checks disk space (must have > 20GB)
- ✅ Performs health checks

**Usage**:
```bash
./deploy/phase-0-pre-migration.sh
```

**Expected output**:
```
✓ All services running
✓ ClickHouse backup completed
✓ Baseline metrics saved
✓ Health checks: 5/5 passed
✓ Phase 0 COMPLETE - Ready for Phase 1
```

---

### Phase 1: Parallel Operation
**File**: `deploy/phase-1-parallel-operation.sh`
**What it does**:
- Enables both NATS and ClickHouse
- Restarts gateways with rolling restart (zero-downtime)
- Verifies both systems receiving events
- Checks error rates and anomaly detection

**Usage**:
```bash
# Start parallel operation
./deploy/phase-1-parallel-operation.sh start

# Verify it's working
./deploy/phase-1-parallel-operation.sh verify
```

**Expected output**:
```
✓ NATS processing: 4500 events/sec
✓ ClickHouse receiving: 1200 events in last 5m
✓ Anomalies detected: 0.5/sec
✓ Error rate: 0.05/sec
✓ Parallel Operation VERIFIED
```

---

## ✅ Verification Checklist

### Before Phase 0
- [ ] SSH access to `root@167.235.30.106` working
- [ ] Can run commands remotely via SSH
- [ ] Docker Compose running on server
- [ ] All 10 services up: `docker-compose -f docker-compose.prod.yml ps`

### Before Phase 1
- [ ] Phase 0 script completed successfully
- [ ] ClickHouse backup created
- [ ] Baseline metrics documented
- [ ] All 5 health checks passed

### Before Phase 2
- [ ] 24+ hours of stable parallel operation
- [ ] Processing rate stable (12,000-16,000 events/sec)
- [ ] Error rate < 0.1%
- [ ] No memory leaks (memory stable)

### Before Phase 3 Switchover
- [ ] Phase 2 integration tests passing
- [ ] Load test successful (14,600 events/sec sustained)
- [ ] Grafana dashboard showing metrics
- [ ] All alert rules configured
- [ ] Team approval obtained
- [ ] Maintenance window scheduled

---

## 🚨 If Something Goes Wrong

### Quick Diagnosis
```bash
# Check service health
docker-compose -f docker-compose.prod.yml ps

# View logs for specific service
docker-compose -f docker-compose.prod.yml logs -f gateway_1

# Check if NATS is running
nc -zv localhost 4222

# Check Prometheus metrics
curl -s http://localhost:9090/api/v1/targets
```

### Common Issues

| Issue | Solution | Guide |
|-------|----------|-------|
| NATS not running | `docker-compose -f docker-compose.prod.yml up -d nats` | TROUBLESHOOTING.md |
| High error rate (>1%) | Check logs, restart gateway | TROUBLESHOOTING.md |
| Low processing rate | Check NATS connection | TROUBLESHOOTING.md |
| Disk full | Clear old data or expand disk | TROUBLESHOOTING.md |

**For detailed troubleshooting**: See `docs/TROUBLESHOOTING.md` (8 detailed guides)

---

## 🔄 Rollback Procedures

### Quick Rollback (< 5 minutes)
If Phase 0 or Phase 1 fails:

```bash
# Disable NATS
sed -i 's/enabled = true  # NATS/enabled = false  # Rollback/' config/default.toml

# Enable ClickHouse
sed -i 's/enabled = false  # ClickHouse/enabled = true/' config/default.toml

# Restart gateways
docker-compose -f docker-compose.prod.yml restart gateway_1 gateway_2 gateway_3 gateway_4

# Verify ClickHouse receiving data
curl 'http://localhost:9090/api/v1/query?query=clickhouse_ingest_total'
```

**For complete rollback procedures**: See `docs/MIGRATION_FROM_CLICKHOUSE.md` Fallback section

---

## 📊 Success Metrics

### Processing Rate
- **Expected**: 14,600 events/sec
- **Acceptable**: 12,000-16,000 events/sec
- **Alert threshold**: < 10,000 events/sec

### Error Rate
- **Expected**: < 0.1%
- **Warning**: 0.1% - 1%
- **Critical**: > 1%

### Storage Growth
- **Before**: 24GB/day (87% reduction target)
- **After**: ~3GB/day
- **Goal**: 3-5GB/day

### Memory Usage
- **Per gateway**: < 2GB
- **Alert**: > 2.5GB
- **Critical**: > 3GB

### Latency (Event → Metrics)
- **Target**: < 500ms
- **Real-time detection**: < 10ms in-memory

---

## 🎯 Key Milestones

| Milestone | Phase | When | Success = |
|-----------|-------|------|-----------|
| Pre-migration complete | 0 | Day 0 | All checks pass ✅ |
| Parallel operation running | 1 | Day 1-2 | Both systems stable ✅ |
| All tests passing | 2 | Day 2-3 | Integration tests OK ✅ |
| Switchover complete | 3 | Day 3 | ClickHouse disabled ✅ |
| Stable & monitored | 4 | Day 4-10 | 7 days green ✅ |

---

## 📞 Need Help?

1. **First**: Check `docs/TROUBLESHOOTING.md` (8 detailed guides)
2. **Second**: Review `docs/MIGRATION_FROM_CLICKHOUSE.md` (complete procedures)
3. **Third**: Check logs: `docker-compose -f docker-compose.prod.yml logs -f <service>`
4. **Escalate**: If Phase fails → immediate rollback (< 5 min) and reschedule

---

## 🎉 Success = Stable NATS-Only System

When complete:
- ✅ 87% storage reduction (3GB/day vs 24GB/day)
- ✅ Real-time anomaly detection (< 10ms)
- ✅ No dependency on ClickHouse
- ✅ Scalable pub/sub architecture
- ✅ Production-ready monitoring (Grafana + Prometheus)
- ✅ 8 production alert rules configured

---

## 📋 Final Checklist Before Starting

- [ ] Read DEPLOYMENT_STATUS.md (10 min)
- [ ] Verify SSH access to production server
- [ ] Confirm disk space available (> 20GB)
- [ ] Review timeline (4 days total)
- [ ] Communicate maintenance window to team
- [ ] Have rollback procedures ready (< 5 min)
- [ ] Start Phase 0: `./deploy/phase-0-pre-migration.sh`

---

**When ready**: Execute `./deploy/phase-0-pre-migration.sh` on production server

**Status**: ✅ READY FOR IMMEDIATE DEPLOYMENT
