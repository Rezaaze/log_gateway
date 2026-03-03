-- ClickHouse initialization script for BGP events storage
-- Fully idempotent: uses IF NOT EXISTS for all operations

CREATE DATABASE IF NOT EXISTS bgp;

CREATE TABLE IF NOT EXISTS bgp.bgp_events (
    timestamp   DateTime64(3, 'UTC'),
    event_type  LowCardinality(String),
    prefix      String,
    origin_as   UInt32,
    as_path     Array(UInt32),
    peer_asn    UInt32,
    peer_ip     String,
    community   Array(String),
    source      LowCardinality(String),
    tenant_id   LowCardinality(String)
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (origin_as, prefix, timestamp)
TTL timestamp + INTERVAL 90 DAY;

CREATE TABLE IF NOT EXISTS bgp.rpki_roa_history (
    timestamp    DateTime64(3, 'UTC'),
    prefix       String,
    max_length   UInt8,
    origin_as    UInt32,
    trust_anchor LowCardinality(String),
    action       LowCardinality(String)
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (timestamp, prefix, origin_as)
TTL timestamp + INTERVAL 365 DAY;

-- Baseline model snapshot tables for cold-start recovery
CREATE TABLE IF NOT EXISTS bgp.baseline_snapshots (
    snapshot_at     DateTime,
    prefix          String,
    ema             Float64,
    variance_ema    Float64,
    sample_count    UInt32
) ENGINE = ReplacingMergeTree(snapshot_at)
ORDER BY (prefix);

CREATE TABLE IF NOT EXISTS bgp.as_knowledge_snapshots (
    snapshot_at     DateTime,
    asn             UInt32,
    days_seen       Array(String)
) ENGINE = ReplacingMergeTree(snapshot_at)
ORDER BY (asn);

-- Alert rules and history (3.1.1)
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
ORDER BY id;

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
TTL fired_at + INTERVAL 180 DAY;

CREATE TABLE IF NOT EXISTS bgp.alert_silences (
    id          UUID DEFAULT generateUUIDv4(),
    fingerprint String,
    reason      String,
    silenced_by String,
    silenced_at DateTime DEFAULT now(),
    expires_at  DateTime,
    active      Bool DEFAULT true
) ENGINE = ReplacingMergeTree(silenced_at)
ORDER BY (id);

-- Tenants (3.4.1)
CREATE TABLE IF NOT EXISTS bgp.tenants (
    id                 UUID,
    name               String,
    api_key_hash       String,
    rate_limit_per_sec UInt32 DEFAULT 1000,
    plan               LowCardinality(String),
    created_at         DateTime64(3, 'UTC'),
    updated_at         DateTime64(3, 'UTC'),
    enabled            UInt8
) ENGINE = ReplacingMergeTree(updated_at)
ORDER BY id;
