# ── BGP Stream — Multi-Stage Build ────────────────────────────────────────────
# Vollständig eigenständig — kein Workspace-Kontext nötig
# ──────────────────────────────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        cmake \
        clang \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/

RUN cargo build --release --locked

# ── Runtime ───────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/bgp-stream /usr/local/bin/bgp-stream

ENV GATEWAY_URL="http://127.0.0.1:8090" \
    WORKERS="8" \
    BATCH_SIZE="100" \
    BATCH_TIMEOUT_MS="20" \
    TENANT_ID="bgp" \
    SAMPLE_RATE="1.0" \
    CHANNEL_CAP="64000" \
    RUST_LOG="warn"

ENTRYPOINT ["/usr/local/bin/bgp-stream"]
