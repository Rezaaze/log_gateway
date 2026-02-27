# ── Stage 1: Builder ─────────────────────────────────────────
# Use Debian slim (glibc) — supports proc-macros on aarch64 (musl does not)
FROM rust:1-slim-bookworm AS builder

# Build dependencies (curl needed by utoipa-swagger-ui build script to download Swagger UI assets)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first — layer cache for dependencies
COPY Cargo.toml Cargo.lock ./

# Dummy main + bench to pre-compile dependencies (benches/ required by Cargo.toml [[bench]])
RUN mkdir -p src benches && \
    echo 'fn main() {}' > src/main.rs && \
    echo 'fn main() {}' > benches/gateway_benchmarks.rs && \
    cargo build --release && \
    rm -rf src benches

# Copy full source
COPY src ./src
COPY config ./config
COPY benches ./benches

# Touch main.rs to invalidate cached dummy binary
RUN touch src/main.rs && cargo build --release

# ── Stage 2: Runtime ─────────────────────────────────────────
# debian:bookworm-slim matches builder glibc version
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    wget \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Binary from builder
COPY --from=builder /app/target/release/log-gateway .

# Default config
COPY --from=builder /app/config ./config

EXPOSE 8080

# Run as non-root
RUN useradd -r -s /bin/false gateway
USER gateway

ENTRYPOINT ["/app/log-gateway"]