# ── Stage 1: Builder ─────────────────────────────────────────
FROM rust:1-alpine AS builder

# musl-dev required for static linking on Alpine
RUN apk add --no-cache musl-dev

# Force fully static binary via musl target
ENV RUSTFLAGS="-C target-feature=+crt-static"

WORKDIR /app

# Copy manifests first — layer cache for dependencies
COPY Cargo.toml Cargo.lock ./

# Dummy main to pre-compile dependencies
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy full source
COPY src ./src
COPY config ./config

# Touch main.rs to invalidate cached dummy binary
RUN touch src/main.rs && cargo build --release

# ── Stage 2: Runtime (distroless) ────────────────────────────
FROM gcr.io/distroless/cc-debian12:nonroot

WORKDIR /app

# Binary from builder
COPY --from=builder /app/target/release/log-gateway .

# Default config
COPY --from=builder /app/config ./config

# data/logs directory is created at runtime via volume mount
# distroless has no shell → no RUN commands allowed here

EXPOSE 8080

ENTRYPOINT ["/app/log-gateway"]