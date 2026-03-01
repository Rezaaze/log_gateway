-- ClickHouse schema for BGP events storage
-- This table stores BGP ANNOUNCE/WITHDRAW events from the log gateway

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

-- Create the database if it doesn't exist
CREATE DATABASE IF NOT EXISTS bgp;