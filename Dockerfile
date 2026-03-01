# ── Stage 1: Builder ─────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY tools/bgp_stream/Cargo.toml ./tools/bgp_stream/Cargo.toml
COPY src/ ./src/
COPY benches/ ./benches/
COPY config/ ./config/
COPY tools/bgp_stream/src/ ./tools/bgp_stream/src/

RUN cargo build --release --locked --package log-gateway

# ── Stage 2: Runtime ─────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    wget \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/log-gateway .
COPY --from=builder /app/config ./config

EXPOSE 8080

RUN useradd -r -s /bin/false gateway
USER gateway

ENTRYPOINT ["/app/log-gateway"]
