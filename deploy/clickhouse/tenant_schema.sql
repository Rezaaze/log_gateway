-- ClickHouse schema for tenant management
-- This schema extends the existing BGP monitoring system with multi-tenancy support

-- Create the database if it doesn't exist (already exists from main schema)
CREATE DATABASE IF NOT EXISTS bgp;

-- Tenants table for storing tenant information and API keys
-- Uses ReplacingMergeTree to handle updates via INSERT with newer updated_at
CREATE TABLE IF NOT EXISTS bgp.tenants (
    id                UUID,
    name              String,
    api_key_hash      String,
    rate_limit_per_sec UInt32 DEFAULT 1000,
    plan              LowCardinality(String),
    created_at        DateTime64(3, 'UTC'),
    updated_at        DateTime64(3, 'UTC'),
    enabled           UInt8
) ENGINE = ReplacingMergeTree(updated_at)
ORDER BY id
COMMENT 'Tenant information with API key hashes and rate limits';

-- Index for efficient tenant lookups by API key hash
ALTER TABLE bgp.tenants ADD INDEX api_key_hash_idx api_key_hash TYPE bloom_filter GRANULARITY 1;

-- Index for efficient tenant lookups by name
ALTER TABLE bgp.tenants ADD INDEX name_idx name TYPE bloom_filter GRANULARITY 1;

-- Index for filtering by enabled status
ALTER TABLE bgp.tenants ADD INDEX enabled_idx enabled TYPE set(2) GRANULARITY 1;

-- Index for filtering by plan
ALTER TABLE bgp.tenants ADD INDEX plan_idx plan TYPE set(10) GRANULARITY 1;