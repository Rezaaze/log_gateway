#!/bin/bash
# ── Zero-Downtime Cluster Deploy ─────────────────────────────────────────────
#
# Rollt das 4-Zylinder Setup auf dem Hetzner-Server aus.
# Der alte Single-Container (port 8080) bleibt während des Deploys erreichbar.
# Nginx übernimmt erst nach erfolgreichem Health-Check aller 4 Instanzen.
#
# Verwendung:
#   ./deploy-cluster.sh              # Vollständiges Deploy
#   ./deploy-cluster.sh --build      # Image neu bauen + Deploy
#   ./deploy-cluster.sh --rollback   # Zurück zum Single-Container
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

COMPOSE_FILE="docker-compose.prod.yml"
IMAGE_NAME="log-gateway:latest"
HEALTH_TIMEOUT=60   # Sekunden bis alle Instanzen healthy sein müssen

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log()     { echo -e "${BLUE}[$(date +%H:%M:%S)]${NC} $*"; }
success() { echo -e "${GREEN}[$(date +%H:%M:%S)] ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}[$(date +%H:%M:%S)] ⚠${NC} $*"; }
error()   { echo -e "${RED}[$(date +%H:%M:%S)] ✗${NC} $*"; exit 1; }

# ── Argument-Parsing ──────────────────────────────────────────────────────────
BUILD=false
ROLLBACK=false
for arg in "$@"; do
  case $arg in
    --build)    BUILD=true ;;
    --rollback) ROLLBACK=true ;;
  esac
done

# ── Rollback: zurück zum Single-Container ─────────────────────────────────────
if [ "$ROLLBACK" = true ]; then
  warn "ROLLBACK: Stoppe Cluster, starte Single-Container..."
  docker compose -f "$COMPOSE_FILE" down --remove-orphans 2>/dev/null || true
  docker compose up -d gateway
  success "Rollback abgeschlossen. Single-Container auf Port 8080."
  exit 0
fi

# ── Voraussetzungen prüfen ────────────────────────────────────────────────────
log "Prüfe Voraussetzungen..."
command -v docker >/dev/null 2>&1 || error "Docker nicht gefunden"
[ -f "$COMPOSE_FILE" ]            || error "$COMPOSE_FILE nicht gefunden"
[ -f "deploy/nginx/nginx.conf" ]  || error "deploy/nginx/nginx.conf nicht gefunden"
[ -d "secrets" ]                  || error "secrets/ Verzeichnis nicht gefunden"
[ -d "data/logs" ]                || mkdir -p data/logs

# ── Image bauen (optional) ────────────────────────────────────────────────────
if [ "$BUILD" = true ]; then
  log "Baue Docker Image ${IMAGE_NAME}..."
  docker build -t "$IMAGE_NAME" . || error "Image Build fehlgeschlagen"
  success "Image gebaut: ${IMAGE_NAME}"
else
  # Prüfen ob Image existiert
  docker image inspect "$IMAGE_NAME" >/dev/null 2>&1 || \
    error "Image ${IMAGE_NAME} nicht gefunden. Führe './deploy-cluster.sh --build' aus."
fi

# ── Aktuellen Status sichern ──────────────────────────────────────────────────
log "Sicherung des aktuellen Status..."
CURRENT_CONTAINER=$(docker ps --format "{{.Names}}" | grep "log-gateway" | head -1 || true)
if [ -n "$CURRENT_CONTAINER" ]; then
  warn "Laufender Container gefunden: ${CURRENT_CONTAINER}"
  warn "Cluster startet parallel — old Container bleibt bis Nginx healthy ist"
fi

# ── Neuen Cluster hochfahren ──────────────────────────────────────────────────
log "Starte Gateway-Cluster (4 Instanzen + Nginx)..."
docker compose -f "$COMPOSE_FILE" up -d \
  gateway_1 gateway_2 gateway_3 gateway_4

# ── Warten bis alle 4 Instanzen healthy sind ─────────────────────────────────
log "Warte auf Health-Checks (max ${HEALTH_TIMEOUT}s)..."
DEADLINE=$(($(date +%s) + HEALTH_TIMEOUT))
ALL_HEALTHY=false

while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  HEALTHY_COUNT=0
  for i in 1 2 3 4; do
    STATUS=$(docker inspect --format='{{.State.Health.Status}}' "gateway_${i}" 2>/dev/null || echo "missing")
    if [ "$STATUS" = "healthy" ]; then
      HEALTHY_COUNT=$((HEALTHY_COUNT + 1))
    fi
  done

  if [ "$HEALTHY_COUNT" -eq 4 ]; then
    ALL_HEALTHY=true
    break
  fi

  echo -ne "\r  Healthy: ${HEALTHY_COUNT}/4 ..."
  sleep 2
done
echo ""

if [ "$ALL_HEALTHY" = false ]; then
  error "Nicht alle Instanzen sind healthy nach ${HEALTH_TIMEOUT}s. Führe Rollback durch..."
fi
success "Alle 4 Gateway-Instanzen sind healthy"

# ── Nginx starten ─────────────────────────────────────────────────────────────
log "Starte Nginx Load Balancer..."
docker compose -f "$COMPOSE_FILE" up -d nginx

# Nginx health prüfen
sleep 3
NGINX_STATUS=$(docker inspect --format='{{.State.Health.Status}}' "nginx-lb" 2>/dev/null || echo "unknown")
if [ "$NGINX_STATUS" != "healthy" ]; then
  warn "Nginx noch nicht healthy (Status: ${NGINX_STATUS}), warte weitere 10s..."
  sleep 10
fi

# Smoke-Test: Anfrage durch Nginx
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
  -X POST http://localhost:8090/api/v1/logs \
  -H "Content-Type: application/json" \
  -d '{"source":"deploy-test","level":"info","message":"cluster smoke test"}' || echo "000")

if [ "$HTTP_CODE" != "202" ] && [ "$HTTP_CODE" != "429" ]; then
  error "Smoke-Test fehlgeschlagen! HTTP ${HTTP_CODE} von Nginx. Rollback empfohlen: ./deploy-cluster.sh --rollback"
fi
success "Smoke-Test bestanden (HTTP ${HTTP_CODE} durch Nginx)"

# ── Alten Single-Container stoppen ────────────────────────────────────────────
if [ -n "$CURRENT_CONTAINER" ] && [ "$CURRENT_CONTAINER" = "log-gateway" ]; then
  log "Stoppe alten Single-Container..."
  docker stop log-gateway 2>/dev/null || true
  docker rm log-gateway 2>/dev/null || true
  success "Alter Container gestoppt"
fi

# ── Restliche Services hochfahren ─────────────────────────────────────────────
log "Starte Support-Services (Prometheus, Grafana, MinIO, Alertmanager)..."
docker compose -f "$COMPOSE_FILE" up -d \
  prometheus grafana minio alertmanager

# ── Abschluss-Status ──────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}════════════════════════════════════════════${NC}"
echo -e "${GREEN}  Cluster erfolgreich deployed!${NC}"
echo -e "${GREEN}════════════════════════════════════════════${NC}"
echo ""
echo "  Endpoints:"
echo "    Log Ingest:  http://$(hostname -I | awk '{print $1}'):8090/api/v1/logs"
echo "    Health:      http://$(hostname -I | awk '{print $1}'):8090/health"
echo "    Prometheus:  http://$(hostname -I | awk '{print $1}'):9090"
echo "    Grafana:     http://$(hostname -I | awk '{print $1}'):3000  (admin/admin)"
echo ""
echo "  Container-Status:"
for i in 1 2 3 4; do
  STATUS=$(docker inspect --format='{{.State.Health.Status}}' "gateway_${i}" 2>/dev/null || echo "unknown")
  CPU=$(docker inspect --format='{{.HostConfig.CpusetCpus}}' "gateway_${i}" 2>/dev/null || echo "?")
  echo "    gateway_${i} (CPU ${CPU}): ${STATUS}"
done
echo ""
echo "  Rollback: ./deploy-cluster.sh --rollback"
