-- ClickHouse schema for BaselineModel cold-start snapshots
-- These tables store snapshots of the baseline model state for cold-start recovery

CREATE TABLE IF NOT EXISTS {db}.baseline_snapshots (
    snapshot_at     DateTime,
    prefix          String,
    ema             Float64,
    variance_ema    Float64,
    sample_count    UInt32
) ENGINE = ReplacingMergeTree(snapshot_at)
ORDER BY (prefix);

CREATE TABLE IF NOT EXISTS {db}.as_knowledge_snapshots (
    snapshot_at     DateTime,
    asn             UInt32,
    days_seen       Array(String)
) ENGINE = ReplacingMergeTree(snapshot_at)
ORDER BY (asn);