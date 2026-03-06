#!/bin/bash
# Phase 0: Pre-Migration Setup Script
# Runs on production server to prepare for NATS migration
# Usage: ./deploy/phase-0-pre-migration.sh

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
LOG_DIR="/root/log-gateway-backup/$TIMESTAMP"
COMPOSE_FILE="docker-compose.prod.yml"

echo -e "${BLUE}════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Phase 0: Pre-Migration Setup${NC}"
echo -e "${BLUE}  Started: $(date)${NC}"
echo -e "${BLUE}════════════════════════════════════════════${NC}"

# Create backup directory
echo -e "\n${BLUE}[1/8]${NC} Creating backup directory..."
mkdir -p "$LOG_DIR"
echo "Backup directory: $LOG_DIR" | tee "$LOG_DIR/phase0.log"

# Check docker-compose
echo -e "\n${BLUE}[2/8]${NC} Checking Docker Compose..."
if ! command -v docker-compose &> /dev/null && ! command -v docker &> /dev/null; then
    echo -e "${RED}✗ Docker not found${NC}" | tee -a "$LOG_DIR/phase0.log"
    exit 1
fi
echo -e "${GREEN}✓ Docker available${NC}" | tee -a "$LOG_DIR/phase0.log"

# Verify services running
echo -e "\n${BLUE}[3/8]${NC} Verifying services are running..."
cd /root/log-gateway

# Check all required services
REQUIRED_SERVICES=("nats" "prometheus" "grafana" "clickhouse" "gateway_1" "gateway_2" "gateway_3" "gateway_4")
FAILED=0

for service in "${REQUIRED_SERVICES[@]}"; do
    if docker-compose -f "$COMPOSE_FILE" ps "$service" | grep -q "Up"; then
        echo -e "${GREEN}✓${NC} $service running" | tee -a "$LOG_DIR/phase0.log"
    else
        echo -e "${RED}✗${NC} $service NOT running" | tee -a "$LOG_DIR/phase0.log"
        FAILED=$((FAILED + 1))
    fi
done

if [ $FAILED -gt 0 ]; then
    echo -e "${RED}ERROR: $FAILED services not running. Start them first:${NC}" | tee -a "$LOG_DIR/phase0.log"
    echo "docker-compose -f docker-compose.prod.yml up -d" | tee -a "$LOG_DIR/phase0.log"
    exit 1
fi

# Backup ClickHouse data
echo -e "\n${BLUE}[4/8]${NC} Backing up ClickHouse data..."
echo "This may take a few minutes..." | tee -a "$LOG_DIR/phase0.log"

if docker exec clickhouse clickhouse-client --query "BACKUP TABLE bgp TO 'file:///backups/pre-phase1'" >> "$LOG_DIR/phase0.log" 2>&1; then
    echo -e "${GREEN}✓${NC} ClickHouse backup completed" | tee -a "$LOG_DIR/phase0.log"
else
    echo -e "${YELLOW}⚠${NC} ClickHouse backup warning (may be expected)" | tee -a "$LOG_DIR/phase0.log"
fi

# Document baseline metrics
echo -e "\n${BLUE}[5/8]${NC} Documenting baseline metrics..."

# Prometheus metrics
echo "=== Prometheus Baseline Metrics ===" | tee "$LOG_DIR/prometheus-baseline.json"
curl -s 'http://localhost:9090/api/v1/query?query=detector_events_processed_total' | jq . >> "$LOG_DIR/prometheus-baseline.json" 2>/dev/null || echo "Could not query Prometheus" >> "$LOG_DIR/prometheus-baseline.json"

# ClickHouse event count
echo "=== ClickHouse Baseline ===" | tee "$LOG_DIR/clickhouse-baseline.log"
docker exec clickhouse clickhouse-client --query "SELECT COUNT() as total_events, formatReadableSize(sum(bytes)) as total_size FROM bgp.bgp_stream FORMAT Pretty;" >> "$LOG_DIR/clickhouse-baseline.log" 2>&1 || echo "Could not query ClickHouse" >> "$LOG_DIR/clickhouse-baseline.log"

echo -e "${GREEN}✓${NC} Baseline metrics saved" | tee -a "$LOG_DIR/phase0.log"

# Check disk space
echo -e "\n${BLUE}[6/8]${NC} Checking disk space..."

# Get current disk usage
DISK_USAGE=$(df /root | awk 'NR==2 {print $5}' | sed 's/%//')
DISK_AVAILABLE=$(df /root | awk 'NR==2 {print $4}')

echo "Disk usage: ${DISK_USAGE}%" | tee -a "$LOG_DIR/phase0.log"
echo "Available: $(numfmt --to=iec $((DISK_AVAILABLE * 1024)) 2>/dev/null || echo "$DISK_AVAILABLE KB")" | tee -a "$LOG_DIR/phase0.log"

# Get data directory sizes
echo -e "\n=== Data Directory Sizes ===" | tee -a "$LOG_DIR/phase0.log"
du -sh /root/log-gateway/data 2>/dev/null >> "$LOG_DIR/phase0.log" || echo "Could not measure log-gateway/data" >> "$LOG_DIR/phase0.log"
du -sh /root/log-gateway/clickhouse-data 2>/dev/null >> "$LOG_DIR/phase0.log" || echo "Could not measure ClickHouse data" >> "$LOG_DIR/phase0.log"

if [ "$DISK_USAGE" -gt 90 ]; then
    echo -e "${RED}✗${NC} CRITICAL: Disk usage > 90%" | tee -a "$LOG_DIR/phase0.log"
    exit 1
elif [ "$DISK_USAGE" -gt 80 ]; then
    echo -e "${YELLOW}⚠${NC} WARNING: Disk usage > 80%" | tee -a "$LOG_DIR/phase0.log"
else
    echo -e "${GREEN}✓${NC} Disk space sufficient" | tee -a "$LOG_DIR/phase0.log"
fi

# Verify NATS
echo -e "\n${BLUE}[7/8]${NC} Verifying NATS Jetstream..."

if docker exec nats nats --server localhost:4222 server info > /dev/null 2>&1; then
    echo -e "${GREEN}✓${NC} NATS server responding" | tee -a "$LOG_DIR/phase0.log"

    # Get NATS info
    docker exec nats nats --server localhost:4222 server info 2>/dev/null >> "$LOG_DIR/nats-status.log" || echo "Could not get NATS info" >> "$LOG_DIR/nats-status.log"
else
    echo -e "${YELLOW}⚠${NC} NATS info unavailable (nats CLI may not be in container)" | tee -a "$LOG_DIR/phase0.log"
fi

# Health check summary
echo -e "\n${BLUE}[8/8]${NC} Final health checks..."

CHECKS_PASSED=0
CHECKS_TOTAL=0

# Check NATS listening
CHECKS_TOTAL=$((CHECKS_TOTAL + 1))
if timeout 2 bash -c 'echo ping | nc -w 1 localhost 4222' > /dev/null 2>&1; then
    echo -e "${GREEN}✓${NC} NATS port 4222 listening" | tee -a "$LOG_DIR/phase0.log"
    CHECKS_PASSED=$((CHECKS_PASSED + 1))
else
    echo -e "${RED}✗${NC} NATS port 4222 not responding" | tee -a "$LOG_DIR/phase0.log"
fi

# Check Prometheus
CHECKS_TOTAL=$((CHECKS_TOTAL + 1))
if curl -s http://localhost:9090/api/v1/targets > /dev/null 2>&1; then
    echo -e "${GREEN}✓${NC} Prometheus API responding" | tee -a "$LOG_DIR/phase0.log"
    CHECKS_PASSED=$((CHECKS_PASSED + 1))
else
    echo -e "${RED}✗${NC} Prometheus API not responding" | tee -a "$LOG_DIR/phase0.log"
fi

# Check ClickHouse
CHECKS_TOTAL=$((CHECKS_TOTAL + 1))
if docker exec clickhouse clickhouse-client --query "SELECT 1" > /dev/null 2>&1; then
    echo -e "${GREEN}✓${NC} ClickHouse responding" | tee -a "$LOG_DIR/phase0.log"
    CHECKS_PASSED=$((CHECKS_PASSED + 1))
else
    echo -e "${RED}✗${NC} ClickHouse not responding" | tee -a "$LOG_DIR/phase0.log"
fi

# Summary
echo -e "\n${BLUE}════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Phase 0 Completion Summary${NC}"
echo -e "${BLUE}════════════════════════════════════════════${NC}"
echo -e "Backup location: ${GREEN}$LOG_DIR${NC}" | tee -a "$LOG_DIR/phase0.log"
echo -e "Health checks: ${GREEN}$CHECKS_PASSED/$CHECKS_TOTAL${NC} passed" | tee -a "$LOG_DIR/phase0.log"
echo -e "Log file: ${GREEN}$LOG_DIR/phase0.log${NC}"
echo -e "\n${YELLOW}Next Steps:${NC}"
echo "1. Review Phase 0 log: $LOG_DIR/phase0.log"
echo "2. If all checks passed, proceed to Phase 1 (Parallel Operation)"
echo "3. Follow docs/MIGRATION_FROM_CLICKHOUSE.md Phase 1 section"

if [ "$CHECKS_PASSED" -eq "$CHECKS_TOTAL" ]; then
    echo -e "\n${GREEN}✓ Phase 0 COMPLETE - Ready for Phase 1${NC}"
    exit 0
else
    echo -e "\n${RED}✗ Phase 0 INCOMPLETE - Fix issues before proceeding${NC}"
    exit 1
fi
