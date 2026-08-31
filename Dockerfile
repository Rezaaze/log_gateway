# ── Stage 1: Builder ─────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

# Cache-buster: pass BUILD_TIMESTAMP as --build-arg to invalidate cache on each build
ARG BUILD_TIMESTAMP=unknown
RUN echo "Building at ${BUILD_TIMESTAMP}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
# Alle Workspace-Member Cargo.toml kopieren (cargo braucht sie zum Workspace-Laden)
COPY tools/bgp_stream/Cargo.toml ./tools/bgp_stream/Cargo.toml
COPY tools/mrt_replay/Cargo.toml ./tools/mrt_replay/Cargo.toml
COPY tools/baseline_builder/Cargo.toml ./tools/baseline_builder/Cargo.toml
# Stub-Sources für Workspace-Member (nicht gebaut, nur für Workspace-Auflösung)
RUN mkdir -p tools/bgp_stream/src tools/mrt_replay/src tools/baseline_builder/src \
    && echo 'fn main(){}' > tools/bgp_stream/src/main.rs \
    && echo 'fn main(){}' > tools/mrt_replay/src/main.rs \
    && echo 'fn main(){}' > tools/baseline_builder/src/main.rs
COPY src/ ./src/
COPY benches/ ./benches/
COPY config/ ./config/
RUN echo "Build timestamp: ${BUILD_TIMESTAMP}" && \
    find ./src -type f -name "*.rs" -exec touch {} \; && \
    cargo build --release --locked --package log-gateway

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
