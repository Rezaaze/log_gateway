# Prometheus Metrics Guide

## Overview

The log-gateway exports Prometheus metrics for monitoring the NATS-based anomaly detection pipeline. Metrics are available at the `/metrics` HTTP endpoint on each gateway instance.

## Detector Metrics

### Counter Metrics

#### `detector_events_processed_total`
- **Type**: Counter
- **Description**: Total number of BGP events processed by the detector from NATS
- **Labels**: None
- **Example**: `detector_events_processed_total`

#### `detector_anomalies_detected_total`
- **Type**: Counter (with labels)
- **Description**: Total number of anomalies detected by type
- **Labels**: `type` (values: `hijack`, `flapping`)
- **Examples**:
  - `detector_anomalies_detected_total{type="hijack"}`
  - `detector_anomalies_detected_total{type="flapping"}`

#### `detector_processing_errors_total`
- **Type**: Counter
- **Description**: Total number of processing errors during anomaly detection
- **Labels**: None
- **Example**: `detector_processing_errors_total`

## PromQL Queries

### Basic Counters

**Total events processed:**
```promql
detector_events_processed_total
```

**Total hijack anomalies detected:**
```promql
detector_anomalies_detected_total{type="hijack"}
```

**Total flapping anomalies detected:**
```promql
detector_anomalies_detected_total{type="flapping"}
```

**Total anomalies (all types):**
```promql
detector_anomalies_detected_total
```

### Rate Metrics

**Events per second (rate over 5 minutes):**
```promql
rate(detector_events_processed_total[5m])
```

**Hijack detections per minute:**
```promql
rate(detector_anomalies_detected_total{type="hijack"}[1m]) * 60
```

**Flapping detections per minute:**
```promql
rate(detector_anomalies_detected_total{type="flapping"}[1m]) * 60
```

**Total anomalies per minute:**
```promql
(rate(detector_anomalies_detected_total[1m]) * 60)
```

### Error Rate

**Processing errors per second:**
```promql
rate(detector_processing_errors_total[5m])
```

**Error rate as percentage:**
```promql
(
  rate(detector_processing_errors_total[5m]) /
  (rate(detector_events_processed_total[5m]) + 0.001)
) * 100
```

### Anomaly Ratios

**Hijack to flapping ratio:**
```promql
rate(detector_anomalies_detected_total{type="hijack"}[5m]) /
(rate(detector_anomalies_detected_total{type="flapping"}[5m]) + 0.001)
```

**Anomaly density (anomalies per 1000 events):**
```promql
(
  rate(detector_anomalies_detected_total[5m]) /
  (rate(detector_events_processed_total[5m]) + 0.001)
) * 1000
```

## Scrape Configuration

The Prometheus server scrapes metrics from all gateway instances configured in `prometheus.prod.yml`:

```yaml
scrape_configs:
  - job_name: "log-gateway"
    scrape_interval: 5s
    static_configs:
      - targets: ["gateway_1:8080", "gateway_2:8080", "gateway_3:8080", "gateway_4:8080"]
    metrics_path: "/metrics"
```

**Scrape Interval**: 5 seconds (for fine-grained metrics)
**Metrics Path**: `/metrics`
**Port**: 8080

## Prometheus Endpoint

### Local Testing

Test metrics export locally:

```bash
curl http://localhost:8080/metrics
```

### Docker Deployment

In the Docker Compose environment:

```bash
curl http://gateway_1:8080/metrics
curl http://gateway_2:8080/metrics
curl http://gateway_3:8080/metrics
curl http://gateway_4:8080/metrics
```

### Example Output

```
# HELP detector_events_processed_total Total number of BGP events processed
# TYPE detector_events_processed_total counter
detector_events_processed_total 14287432

# HELP detector_anomalies_detected_total Total number of anomalies detected
# TYPE detector_anomalies_detected_total counter
detector_anomalies_detected_total{type="hijack"} 342
detector_anomalies_detected_total{type="flapping"} 156

# HELP detector_processing_errors_total Total number of processing errors
# TYPE detector_processing_errors_total counter
detector_processing_errors_total 0
```

## Grafana Panels

See `docs/GRAFANA_DASHBOARDS.md` for Grafana panel configurations using these metrics.

## Integration with Alerting

Example alert rule in `prometheus.rules.yml`:

```yaml
- alert: HighAnomalyDetectionRate
  expr: rate(detector_anomalies_detected_total[5m]) > 0.1
  for: 5m
  annotations:
    summary: "High anomaly detection rate"
```

## Troubleshooting

**No metrics appearing?**
- Verify NATS is enabled: `enabled = true` in config
- Check NATS URL is configured: `url = "nats://nats:4222"`
- Verify Prometheus scrape targets are reachable: `curl http://gateway_1:8080/metrics`

**Metrics are zero?**
- Check if BGP events are flowing from bgp-stream
- Verify detector runner is running: Check logs for "Detector runner started"
- Test with: `curl http://localhost:8080/metrics | grep detector_`

**Missing anomalies?**
- Check detector configuration is enabled
- Verify BGP prefixes have enough historical context (7-day warmup for HijackDetector)
- Review anomaly detection logs: `docker logs log_gateway_1`
