CREATE DATABASE IF NOT EXISTS bgp;

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