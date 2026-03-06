# Troubleshooting Guide - NATS Streaming Architecture

## Quick Diagnosis

### Symptom: No BGP events being processed

**Quick Check**:
```bash
# 1. Check detector metrics
curl http://localhost:8080/metrics | grep detector_events

# 2. Check NATS connection
docker logs log_gateway_1 | grep -i nats | tail -5

# 3. Check BGP-Stream publishing
docker logs log_bgp_stream | grep -i "publish\|error" | tail -5

# 4. Verify NATS is running
curl http://nats:4222
```

**Common Causes**:
| Cause | Fix |
|-------|-----|
| NATS not running | `docker-compose up -d nats` |
| BGP-Stream stopped | `docker-compose up -d bgp-stream` |
| Wrong NATS URL in config | Update config/default.toml [nats] section |
| NATS disabled | Set `[nats] enabled = true` in config |

---

## Detailed Troubleshooting

### Issue 1: "Detector runner not started" in logs

**Error Message**:
```
WARN: NATS enabled but no URL configured, detector runner started without NATS
```

**Root Cause**: NATS URL is empty or invalid

**Solution**:
```toml
# config/default.toml
[nats]
enabled = true
url = "nats://nats:4222"  # Fixed: was empty
subject = "bgp.events"
```

**Verify**:
```bash
docker-compose restart gateway_1
docker logs log_gateway_1 | grep "Detector runner"
```

---

### Issue 2: NATS subscription connection timeout

**Error Message**:
```
ERROR: NATS subscriber failed: connection timeout
```

**Root Cause**: Network issue or NATS not reachable

**Diagnosis**:
```bash
# Check NATS service
docker ps | grep nats

# Test connectivity from gateway
docker exec log_gateway_1 curl http://nats:4222

# Check NATS logs
docker logs log_nats | grep -i error | tail -10
```

**Solutions**:
1. **NATS not running**:
   ```bash
   docker-compose up -d nats
   sleep 5
   ```

2. **Network issue**:
   ```bash
   # Verify network exists
   docker network ls | grep log_

   # Reconnect to network
   docker-compose down
   docker-compose up -d
   ```

3. **Port conflict**:
   ```bash
   # Check if 4222 is in use
   netstat -tulpn | grep 4222

   # Change NATS port in docker-compose.prod.yml if needed
   ```

---

### Issue 3: Anomalies not being detected

**Symptom**: `detector_anomalies_detected_total` not increasing

**Root Cause**: Detectors need warmup period

**Explanation**:
- HijackDetector tracks prefix → origin ASN mappings
- Needs ~1000 announcements per prefix before detecting changes
- First 7 days = "learning period"

**Workaround**:
```bash
# Simulate some history (during testing only)
docker exec log_gateway_1 curl -X POST \
  http://localhost:8080/api/v1/logs/batch \
  -H "Content-Type: application/json" \
  -d '{"logs": [...]}'  # Batch of historical BGP records
```

**Expected Timeline**:
- Day 1-7: Learning phase, few/no anomalies detected
- Day 7+: Production accuracy achieved

---

### Issue 4: High memory usage

**Symptom**: Container memory grows over time

**Check Memory**:
```bash
docker stats gateway_1 --no-stream

# Monitor for 5 minutes
for i in {1..5}; do
  docker stats gateway_1 --no-stream
  sleep 60
done
```

**Root Causes & Fixes**:

| Cause | Memory Impact | Fix |
|-------|---------------|-----|
| Long-running tasks | +500MB/hour | Restart gateway daily |
| Detector state accumulation | +1GB/week | Clear prefix cache |
| NATS message backlog | +100MB/10k msgs | Increase channel size or reduce rate |
| Prometheus label explosion | +200MB+ | Limit label cardinality |

**Clear Detector State**:
```bash
# Detector state is in-memory only, restart to clear
docker-compose restart gateway_1 gateway_2 gateway_3 gateway_4
```

---

### Issue 5: Prometheus metrics not updating

**Symptom**: Metrics stuck at same value

**Diagnosis**:
```bash
# Check metrics endpoint
curl -v http://localhost:8080/metrics | head -50

# Check Prometheus scrape targets
curl http://localhost:9090/api/v1/targets | jq '.data.activeTargets | map(select(.health=="up"))'

# Check last scrape time
curl 'http://localhost:9090/api/v1/targets' | jq '.data.activeTargets[0].lastScrapeTime'
```

**Common Issues**:

1. **Gateway not responding**:
   ```bash
   curl http://localhost:8080/metrics
   # Expected: 200 OK with metrics
   ```

2. **Prometheus not scraping**:
   ```bash
   # Check scrape config
   curl http://localhost:9090/api/v1/config | jq '.data.yaml' | grep -A 5 "log-gateway"
   ```

3. **Metrics endpoint down**:
   ```bash
   docker logs log_gateway_1 | grep metrics | tail -5
   docker restart log_gateway_1
   ```

---

### Issue 6: AlertManager not sending alerts

**Symptom**: Alerts fire in Prometheus but not in Slack/Email

**Diagnosis**:
```bash
# Check AlertManager config
docker exec log_alertmanager cat /etc/alertmanager/alertmanager.yml

# Check AlertManager logs
docker logs log_alertmanager | grep -i "route\|webhook\|error" | tail -20

# Send test alert
curl -X POST http://localhost:9093/api/v1/alerts \
  -H "Content-Type: application/json" \
  -d '[{"labels":{"alertname":"TestAlert","severity":"warning"}}]'
```

**Solutions**:

1. **Missing webhook URL**:
   ```yaml
   # alertmanager.yml
   global:
     slack_api_url: 'https://hooks.slack.com/services/YOUR/WEBHOOK'
   ```

2. **Route not configured**:
   ```yaml
   route:
     receiver: 'default'
     group_by: ['alertname']

   receivers:
     - name: 'default'
       slack_configs:
         - channel: '#alerts'
   ```

3. **Alert not matching route**:
   ```bash
   # List active alerts
   curl http://localhost:9093/api/v1/alerts | jq '.data[] | {alertname, labels}'
   ```

---

### Issue 7: BGP-Stream WebSocket timeout

**Error Message**:
```
ERROR: WebSocket connection timeout to ris-live.ripe.net
```

**Root Cause**: RIS Live WebSocket blocked or DNS issue

**Diagnosis**:
```bash
# Check DNS
docker exec log_bgp_stream getent hosts ris-live.ripe.net

# Check network access
docker exec log_bgp_stream curl https://ris-live.ripe.net/v1/ws/

# Check /etc/hosts
docker exec log_bgp_stream cat /etc/hosts | grep ris-live
```

**Solutions**:

1. **Network blocked**: Add IPv4 route
   ```bash
   # In docker-compose.prod.yml, add to bgp-stream:
   network_mode: "host"
   ```

2. **DNS not resolving**: Add to /etc/hosts
   ```
   193.0.11.16 ris-live.ripe.net
   ```

3. **IPv6 issue**: Disable IPv6
   ```bash
   docker exec log_bgp_stream sysctl -w net.ipv6.conf.all.disable_ipv6=1
   ```

---

### Issue 8: Detector processing errors increasing

**Symptom**: `detector_processing_errors_total` > 0

**Diagnosis**:
```bash
# Check error logs
docker logs log_gateway_1 | grep -i "error\|exception" | tail -20

# Check error rate
curl 'http://localhost:9090/api/v1/query?query=rate(detector_processing_errors_total[5m])'
```

**Common Errors & Fixes**:

| Error | Cause | Fix |
|-------|-------|-----|
| "Invalid BGP record" | Malformed NATS message | Check bgp-stream output format |
| "Detector check failed" | Memory pressure | Increase container memory |
| "Channel send failed" | Detector too slow | Increase channel buffer size |
| "Timeout in detector" | Complex record | Check AS path length |

---

## Performance Tuning

### Increase Processing Rate

**Current Target**: 14,600 events/sec

**Bottleneck Analysis**:
```bash
# Check CPU usage
docker stats gateway_1 --no-stream | awk '{print $3}'  # %CPU

# Check memory usage
docker stats gateway_1 --no-stream | awk '{print $4}'  # Memory

# Check channel depth
docker logs log_gateway_1 | grep "channel full" | wc -l
```

**Tuning Options**:

1. **Increase channel buffer** (src/lib.rs):
   ```rust
   let (detector_tx, detector_rx) =
       tokio::sync::mpsc::channel::<BgpRecord>(128000);  // Was 64k
   ```

2. **Increase gateway instances**:
   ```yaml
   # docker-compose.prod.yml
   gateway_5:
     image: ghcr.io/rezaaze/log_gateway:latest
     ports:
       - "8084:8080"
   ```

3. **Optimize detector** (src/anomaly_detector.rs):
   - Reduce prefix_to_asns map size
   - Use bounded prefix cache
   - Add rate limiting

### Reduce Latency

**Target**: < 500ms event → metrics update

**Optimization**:
```bash
# Reduce Prometheus scrape interval
# deploy/prometheus.prod.yml:
scrape_interval: 5s  # From 15s

# Reduce recording rule interval
global:
  evaluation_interval: 5s  # From 15s
```

---

## Monitoring Queries

### Key Metrics to Monitor

```promql
# Throughput
rate(detector_events_processed_total[5m])

# Error rate
(rate(detector_processing_errors_total[5m]) / rate(detector_events_processed_total[5m])) * 100

# Anomaly rate
rate(detector_anomalies_detected_total[5m])

# Per-type anomaly breakdown
rate(detector_anomalies_detected_total{type="hijack"}[5m])
rate(detector_anomalies_detected_total{type="flapping"}[5m])
```

### Alerting Thresholds

| Alert | Threshold | Action |
|-------|-----------|--------|
| No events | 0 events/sec for 5m | Check NATS connection |
| Low rate | < 1k events/sec for 10m | Check BGP-Stream |
| High errors | > 1% error rate for 5m | Check logs, restart gateway |
| High anomalies | > 0.5 anomalies/sec for 5m | Investigate network |

---

## Rollback to ClickHouse

If issues cannot be resolved, rollback to ClickHouse-only mode:

```bash
# 1. Disable NATS in config
# config/default.toml
[nats]
enabled = false  # Disable streaming

# 2. Keep ClickHouse enabled
[clickhouse]
enabled = true

# 3. Restart gateways
docker-compose restart gateway_1 gateway_2 gateway_3 gateway_4

# 4. Verify ClickHouse receiving data
curl 'http://localhost:9090/api/v1/query?query=clickhouse_ingest_total' | jq '.data.result'
```

---

## Getting Help

### Log Collection

```bash
# Collect all relevant logs
mkdir -p /tmp/log-gateway-debug
docker logs log_gateway_1 > /tmp/log-gateway-debug/gateway_1.log
docker logs log_nats > /tmp/log-gateway-debug/nats.log
docker logs log_bgp_stream > /tmp/log-gateway-debug/bgp-stream.log
docker logs log_prometheus > /tmp/log-gateway-debug/prometheus.log
docker logs log_grafana > /tmp/log-gateway-debug/grafana.log

# System info
docker stats --no-stream > /tmp/log-gateway-debug/docker-stats.txt
df -h > /tmp/log-gateway-debug/disk-usage.txt
```

### Support Resources

- **GitHub Issues**: https://github.com/Rezaaze/log_gateway/issues
- **Documentation**: /root/log-gateway/docs/
- **Configuration**: /root/log-gateway/config/
- **Logs**: `docker logs <container>`

---

**Last Updated**: 2026-03-06
**Architecture**: NATS Jetstream Streaming (v1.0)
