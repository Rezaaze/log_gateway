#!/usr/bin/env bash
# deploy.sh — Log Gateway one-shot deployment script
# Usage: curl -fsSL https://raw.githubusercontent.com/Rezaaze/log_gateway/main/deploy.sh | bash
# Or:    bash deploy.sh [--port PORT] [--dir DIR] [--image IMAGE]
set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
IMAGE="${IMAGE:-ghcr.io/rezaaze/log_gateway:latest}"
INSTALL_DIR="${INSTALL_DIR:-/opt/log-gateway}"
PORT="${PORT:-8080}"
GHCR_TOKEN="${GHCR_TOKEN:-}"
GHCR_USER="${GHCR_USER:-Rezaaze}"

# ── Parse args ────────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --port)       PORT="$2";        shift 2 ;;
    --dir)        INSTALL_DIR="$2"; shift 2 ;;
    --image)      IMAGE="$2";       shift 2 ;;
    --token)      GHCR_TOKEN="$2";  shift 2 ;;
    --ghcr-user)  GHCR_USER="$2";   shift 2 ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# ── Colors ────────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info()    { echo -e "${GREEN}[✓]${NC} $*"; }
warning() { echo -e "${YELLOW}[!]${NC} $*"; }
error()   { echo -e "${RED}[✗]${NC} $*"; exit 1; }

echo ""
echo "╔══════════════════════════════════════╗"
echo "║        Log Gateway Deployment        ║"
echo "╚══════════════════════════════════════╝"
echo ""

# ── 1. Docker check / install ─────────────────────────────────────────────────
info "Checking Docker..."
if ! command -v docker &>/dev/null; then
  warning "Docker not found — installing..."
  curl -fsSL https://get.docker.com | sh
  systemctl enable docker
  systemctl start docker
fi
DOCKER_VERSION=$(docker --version | grep -oP '\d+\.\d+\.\d+' | head -1)
info "Docker $DOCKER_VERSION ready"

# ── 2. Detect free port ───────────────────────────────────────────────────────
info "Checking port availability..."
ORIGINAL_PORT=$PORT
while ss -tlnp 2>/dev/null | grep -q ":${PORT} " || \
      netstat -tlnp 2>/dev/null | grep -q ":${PORT} "; do
  warning "Port $PORT is in use — trying $((PORT + 1))..."
  PORT=$((PORT + 1))
done
if [[ "$PORT" != "$ORIGINAL_PORT" ]]; then
  warning "Using port $PORT instead of $ORIGINAL_PORT"
else
  info "Port $PORT is free"
fi

# ── 3. Detect architecture ────────────────────────────────────────────────────
ARCH=$(uname -m)
info "Architecture: $ARCH"
# Docker multi-arch manifest handles this automatically — no action needed

# ── 4. Create directory structure ─────────────────────────────────────────────
info "Setting up $INSTALL_DIR..."
mkdir -p "$INSTALL_DIR/secrets" "$INSTALL_DIR/data/logs"
chmod 700 "$INSTALL_DIR/secrets"

# ── 5. Generate secrets (only if not already present) ─────────────────────────
info "Checking secrets..."
if [[ ! -s "$INSTALL_DIR/secrets/gateway_api_key.txt" ]]; then
  openssl rand -hex 32 > "$INSTALL_DIR/secrets/gateway_api_key.txt"
  chmod 644 "$INSTALL_DIR/secrets/gateway_api_key.txt"
  info "Generated new API key"
else
  info "API key already exists — keeping it"
fi

if [[ ! -s "$INSTALL_DIR/secrets/gateway_jwt_secret.txt" ]]; then
  openssl rand -hex 64 > "$INSTALL_DIR/secrets/gateway_jwt_secret.txt"
  chmod 644 "$INSTALL_DIR/secrets/gateway_jwt_secret.txt"
  info "Generated new JWT secret"
else
  info "JWT secret already exists — keeping it"
fi

# ── 6. Write docker-compose.yml ───────────────────────────────────────────────
info "Writing docker-compose.yml..."
cat > "$INSTALL_DIR/docker-compose.yml" <<EOF
services:
  gateway:
    image: ${IMAGE}
    container_name: log-gateway
    restart: unless-stopped
    ports:
      - "${PORT}:8080"
    environment:
      - LOG_FORMAT=json
      - GATEWAY__SERVER__HOST=0.0.0.0
      - GATEWAY__SERVER__PORT=8080
      - GATEWAY__SINK__ENABLED=true
      - GATEWAY__SINK__OUTPUT_DIR=/data/logs
      - GATEWAY__SINK__COMPRESS=true
      - GATEWAY__METRICS__ENABLED=true
      - GATEWAY__RATE_LIMIT__ENABLED=true
      - GATEWAY__RATE_LIMIT__REQUESTS_PER_SECOND=100
      - GATEWAY__TLS__ENABLED=false
      - GATEWAY__S3__ENABLED=false
    secrets:
      - gateway_api_key
      - gateway_jwt_secret
    volumes:
      - ./data:/data
    healthcheck:
      test: ["CMD", "wget", "-qO-", "http://localhost:8080/health"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 15s

secrets:
  gateway_api_key:
    file: ./secrets/gateway_api_key.txt
  gateway_jwt_secret:
    file: ./secrets/gateway_jwt_secret.txt
EOF

# ── 7. Pull image ─────────────────────────────────────────────────────────────
info "Pulling image $IMAGE..."
if [[ -n "$GHCR_TOKEN" ]]; then
  info "Logging into GHCR..."
  echo "$GHCR_TOKEN" | docker login ghcr.io -u "$GHCR_USER" --password-stdin
fi
if ! docker pull "$IMAGE"; then
  error "Failed to pull image '$IMAGE'.
  If the image is private, provide a token:
    bash deploy.sh --token <github-pat>
  Or make the GHCR package public at:
    https://github.com/users/Rezaaze/packages/container/log_gateway/settings"
fi

# ── 8. Start / restart ────────────────────────────────────────────────────────
info "Starting gateway..."
cd "$INSTALL_DIR"
docker compose up -d

# ── 9. Health check ───────────────────────────────────────────────────────────
info "Waiting for gateway to become healthy..."
for i in $(seq 1 15); do
  HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" "http://localhost:${PORT}/health" 2>/dev/null || echo "000")
  if [[ "$HTTP_STATUS" == "200" ]]; then
    HEALTH=$(curl -s "http://localhost:${PORT}/health")
    info "Health check passed!"
    break
  fi
  echo "  attempt $i/15 (HTTP $HTTP_STATUS)..."
  sleep 3
done

if [[ "$HTTP_STATUS" != "200" ]]; then
  error "Gateway did not become healthy within 45s. Check: docker logs log-gateway"
fi

# ── 10. Summary ───────────────────────────────────────────────────────────────
SERVER_IP=$(hostname -I | awk '{print $1}' 2>/dev/null || echo "localhost")
API_KEY=$(cat "$INSTALL_DIR/secrets/gateway_api_key.txt")

echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║              🚀 Deployment successful!                       ║"
echo "╠══════════════════════════════════════════════════════════════╣"
printf "║  Health:   http://%-43s║\n" "${SERVER_IP}:${PORT}/health"
printf "║  Metrics:  http://%-43s║\n" "${SERVER_IP}:${PORT}/metrics"
printf "║  Swagger:  http://%-43s║\n" "${SERVER_IP}:${PORT}/swagger-ui/"
echo "╠══════════════════════════════════════════════════════════════╣"
printf "║  API Key:  %-51s║\n" "${API_KEY:0:16}... (${#API_KEY} chars)"
printf "║  Install:  %-51s║\n" "$INSTALL_DIR"
echo "╠══════════════════════════════════════════════════════════════╣"
echo "║  Test:                                                       ║"
echo "║  curl -X POST http://${SERVER_IP}:${PORT}/api/v1/logs \\"
echo "║    -H 'X-API-Key: <your-key>' \\"
echo "║    -H 'Content-Type: application/json' \\"
echo "║    -d '{\"level\":\"info\",\"source\":\"test\",\"message\":\"hello\"}'"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""
info "API key saved at: $INSTALL_DIR/secrets/gateway_api_key.txt"
info "Manage with:      docker compose -C $INSTALL_DIR logs -f"
