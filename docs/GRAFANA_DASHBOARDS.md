# Grafana Dashboard Configuration Guide

## Dashboard: BGP Anomaly Detection Pipeline

This guide provides Grafana panel configurations for monitoring the detector metrics.

## Panels

### 1. Events Processed (Gauge)

**Title**: "Events Processed"
**Type**: Gauge
**Query**:
```promql
detector_events_processed_total
```

**Configuration**:
- Unit: "short"
- Thresholds: 0-100k (green), 100k-1M (yellow), 1M+ (red)
- Min: 0
- Max: Auto

### 2. Events Per Second (Graph)

**Title**: "Processing Rate (events/sec)"
**Type**: Time series
**Query**:
```promql
rate(detector_events_processed_total[5m])
```

**Configuration**:
- Legend: Show
- Decimals: 2
- Min: 0
- Y-axis label: "events/sec"

### 3. Anomalies Detected (Graph)

**Title**: "Anomalies Detected by Type"
**Type**: Time series
**Queries**:
```promql
rate(detector_anomalies_detected_total{type="hijack"}[1m]) * 60
```
Label: "Hijack/min"

```promql
rate(detector_anomalies_detected_total{type="flapping"}[1m]) * 60
```
Label: "Flapping/min"

**Configuration**:
- Legend: Show
- Decimals: 1
- Min: 0
- Y-axis label: "anomalies/min"
- Colors: Red for hijack, Orange for flapping

### 4. Total Anomalies Counter

**Title**: "Total Anomalies"
**Type**: Stat (big value)
**Query**:
```promql
detector_anomalies_detected_total
```

**Configuration**:
- Unit: "short"
- Decimals: 0
- Min: 0

### 5. Anomaly Type Distribution (Pie Chart)

**Title**: "Anomaly Distribution"
**Type**: Pie chart
**Query**:
```promql
detector_anomalies_detected_total
```

**Configuration**:
- Legend: Show
- Display mode: Pie chart
- Values: All

### 6. Processing Errors (Graph)

**Title**: "Processing Errors"
**Type**: Time series
**Query**:
```promql
rate(detector_processing_errors_total[5m])
```

**Configuration**:
- Legend: Show
- Decimals: 2
- Min: 0
- Y-axis label: "errors/sec"
- Color: Red

### 7. Error Rate (Stat)

**Title**: "Error Rate (%)"
**Type**: Stat
**Query**:
```promql
(rate(detector_processing_errors_total[5m]) / (rate(detector_events_processed_total[5m]) + 0.001)) * 100
```

**Configuration**:
- Unit: "percent"
- Decimals: 2
- Min: 0
- Max: 100
- Thresholds: 0 (green), 1 (yellow), 5 (red)

### 8. Anomaly Density (Stat)

**Title**: "Anomaly Density"
**Type**: Stat
**Query**:
```promql
(rate(detector_anomalies_detected_total[5m]) / (rate(detector_events_processed_total[5m]) + 0.001)) * 1000
```

**Configuration**:
- Unit: "short"
- Decimals: 2
- Min: 0
- Label: "anomalies per 1k events"

### 9. Per-Instance Comparison (Table)

**Title**: "Gateway Instance Metrics"
**Type**: Table
**Query**:
```promql
{__name__=~"detector_.*", instance=~".*"}
```

**Configuration**:
- Show: All values
- Sort by: Value (descending)

### 10. 24-Hour Comparison

**Title**: "24h Trend"
**Type**: Time series
**Query**:
```promql
rate(detector_events_processed_total[24h])
```

**Configuration**:
- Legend: Show
- Time range: Last 24 hours
- Interval: 1h

## Dashboard JSON

Complete dashboard JSON can be provisioned in Grafana via:

1. **Manual Import**:
   - Grafana → Dashboards → Import
   - Paste JSON or upload file
   - Select Prometheus datasource
   - Click Import

2. **Automatic Provisioning** (Docker):
   Place in `deploy/grafana/provisioning/dashboards/`
   ```yaml
   # dashboard.yml
   apiVersion: 1
   providers:
     - name: 'BGP Detection'
       orgId: 1
       folder: 'BGP'
       type: file
       disableDeletion: false
       updateIntervalSeconds: 10
       allowUiUpdates: true
       options:
         path: /etc/grafana/provisioning/dashboards
   ```

## Alert Rules

Add to `prometheus.rules.yml`:

```yaml
groups:
  - name: bgp_detector
    interval: 30s
    rules:
      - alert: NoEventsProcessed
        expr: rate(detector_events_processed_total[5m]) == 0
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "No BGP events processed"

      - alert: HighAnomalyRate
        expr: rate(detector_anomalies_detected_total[5m]) > 0.5
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High anomaly detection rate"

      - alert: ProcessingErrors
        expr: rate(detector_processing_errors_total[5m]) > 0
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Detector processing errors"
```

## Template Variables

For multi-instance dashboards, add template variables:

**Variable: instance**
- Type: Query
- Data source: Prometheus
- Query: `label_values(detector_events_processed_total, instance)`
- Refresh: On time range change
- Multi-select: enabled

Then use in queries:
```promql
detector_events_processed_total{instance="$instance"}
```

## Dashboard Export/Import

**Export current dashboard:**
1. Dashboard settings → JSON model
2. Copy entire JSON
3. Save to file
4. Commit to repository

**Import dashboard:**
1. Grafana → Create → Import
2. Upload JSON file
3. Select Prometheus datasource
4. Click Import

## Troubleshooting

**Panels show no data?**
- Check Prometheus datasource is configured
- Verify queries are correct in Prometheus UI
- Check time range covers when detector started

**Metrics missing?**
- Verify detector is running: check logs
- Verify NATS is enabled and connected
- Check `/metrics` endpoint directly: `curl http://localhost:8080/metrics`

**Slow queries?**
- Increase scrape interval in prometheus.prod.yml (default 5s)
- Use longer time ranges in queries
- Add rate() with longer intervals like `[10m]`
