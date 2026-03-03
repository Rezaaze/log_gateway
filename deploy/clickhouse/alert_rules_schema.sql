-- ClickHouse schema for alert rules and alert history
-- This schema extends the existing BGP monitoring system with alert management

-- Create the database if it doesn't exist (already exists from main schema)
CREATE DATABASE IF NOT EXISTS bgp;

-- Alert rules table for storing user-defined alert rules
-- Uses ReplacingMergeTree to handle updates via INSERT with newer updated_at
CREATE TABLE IF NOT EXISTS bgp.alert_rules (
    id          UUID DEFAULT generateUUIDv4(),
    name        String,
    description String,
    rule_type   LowCardinality(String),
    threshold   Float64,
    enabled     Bool DEFAULT true,
    tenant_id   String DEFAULT '',
    created_at  DateTime DEFAULT now(),
    updated_at  DateTime DEFAULT now()
) ENGINE = ReplacingMergeTree(updated_at)
ORDER BY id
COMMENT 'Alert rules for BGP anomaly detection';

-- Alert history table for storing fired alerts
-- Uses MergeTree for time-series data with efficient time-based queries
CREATE TABLE IF NOT EXISTS bgp.alert_history (
    id          UUID DEFAULT generateUUIDv4(),
    rule_id     UUID,
    alert_type  String,
    prefix      String,
    origin_as   UInt32,
    confidence  Float64,
    status      LowCardinality(String) DEFAULT 'fired',
    fired_at    DateTime64(3) DEFAULT now64(3),
    resolved_at Nullable(DateTime64(3))
) ENGINE = MergeTree()
ORDER BY (fired_at, alert_type)
TTL fired_at + INTERVAL 180 DAY
COMMENT 'History of fired alerts with resolution tracking';

-- Create materialized view for active alerts (status = 'fired')
-- This provides a fast view of currently active alerts
CREATE MATERIALIZED VIEW IF NOT EXISTS bgp.active_alerts
ENGINE = MergeTree()
ORDER BY (fired_at, alert_type) AS
SELECT
    id,
    rule_id,
    alert_type,
    prefix,
    origin_as,
    confidence,
    status,
    fired_at,
    resolved_at
FROM bgp.alert_history
WHERE status = 'fired'
    AND resolved_at IS NULL;

-- Index for efficient rule lookups by type and enabled status
ALTER TABLE bgp.alert_rules ADD INDEX rule_type_enabled_idx rule_type TYPE set(100) GRANULARITY 1;
ALTER TABLE bgp.alert_rules ADD INDEX enabled_idx enabled TYPE set(2) GRANULARITY 1;

-- Index for efficient alert history queries
ALTER TABLE bgp.alert_history ADD INDEX rule_id_idx rule_id TYPE bloom_filter GRANULARITY 1;
ALTER TABLE bgp.alert_history ADD INDEX prefix_idx prefix TYPE bloom_filter GRANULARITY 1;
ALTER TABLE bgp.alert_history ADD INDEX origin_as_idx origin_as TYPE bloom_filter GRANULARITY 1;
ALTER TABLE bgp.alert_history ADD INDEX status_idx status TYPE set(10) GRANULARITY 1;

-- Silence table for suppressing alerts based on fingerprint
-- Uses ReplacingMergeTree to handle updates via INSERT with newer silenced_at
CREATE TABLE IF NOT EXISTS bgp.alert_silences (
    id          UUID DEFAULT generateUUIDv4(),
    fingerprint String,
    reason      String,
    silenced_by String,
    silenced_at DateTime DEFAULT now(),
    expires_at  DateTime,
    active      Bool DEFAULT true
) ENGINE = ReplacingMergeTree(silenced_at)
ORDER BY (id)
COMMENT 'Silences for suppressing alerts based on fingerprint';

-- Index for efficient silence lookups
ALTER TABLE bgp.alert_silences ADD INDEX fingerprint_idx fingerprint TYPE bloom_filter GRANULARITY 1;
ALTER TABLE bgp.alert_silences ADD INDEX active_idx active TYPE set(2) GRANULARITY 1;
ALTER TABLE bgp.alert_silences ADD INDEX expires_at_idx expires_at TYPE minmax GRANULARITY 1;
