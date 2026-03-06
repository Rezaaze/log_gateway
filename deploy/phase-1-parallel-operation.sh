#!/bin/bash
# Phase 1: Parallel Operation Script
# Enables both NATS and ClickHouse to run simultaneously
# Usage: ./deploy/phase-1-parallel-operation.sh [start|verify|stop]

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

ACTION="${1:-verify}"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
LOG_FILE="/tmp/phase1-parallel-$TIMESTAMP.log"
COMPOSE_FILE="docker-compose.prod.yml"

echo -e "${BLUE}════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Phase 1: Parallel Operation${NC}"
echo -e "${BLUE}  Action: $ACTION${NC}"
echo -e "${BLUE}  Started: $(date)${NC}"
echo -e "${BLUE}════════════════════════════════════════════${NC}"

cd /root/log-gateway

# Function: Start parallel operation
start_parallel() {
    echo -e "\n${BLUE}Starting Parallel Operation...${NC}"

    # Step 1: Ensure both systems are configured
    echo -e "\n${BLUE}[1/4]${NC} Verifying configuration..."

    CONFIG_FILE="config/default.toml"

    if grep -q "^\[nats\]" "$CONFIG_FILE"; then
        echo -e "${GREEN}✓${NC} NATS configuration found" | tee -a "$LOG_FILE"
    else
        echo -e "${RED}✗${NC} NATS configuration missing" | tee -a "$LOG_FILE"
        exit 1
    fi

    if grep -q "^\[clickhouse\]" "$CONFIG_FILE"; then
        echo -e "${GREEN}✓${NC} ClickHouse configuration found" | tee -a "$LOG_FILE"
    else
        echo -e "${RED}✗${NC} ClickHouse configuration missing" | tee -a "$LOG_FILE"
        exit 1
    fi

    # Step 2: Ensure NATS is enabled
    echo -e "\n${BLUE}[2/4]${NC} Enabling NATS in configuration..."

    if grep -A1 "^\[nats\]" "$CONFIG_FILE" | grep -q "enabled = true"; then
        echo -e "${GREEN}✓${NC} NATS already enabled" | tee -a "$LOG_FILE"
    else
        sed -i '/^\[nats\]/,/^$/s/enabled = false/enabled = true/' "$CONFIG_FILE"
        echo -e "${GREEN}✓${NC} NATS enabled in configuration" | tee -a "$LOG_FILE"
    fi

    # Step 3: Keep ClickHouse enabled
    echo -e "\n${BLUE}[3/4]${NC} Ensuring ClickHouse remains enabled..."

    if grep -A1 "^\[clickhouse\]" "$CONFIG_FILE" | grep -q "enabled = true"; then
        echo -e "${GREEN}✓${NC} ClickHouse already enabled" | tee -a "$LOG_FILE"
    else
        sed -i '/^\[clickhouse\]/,/^$/s/enabled = false/enabled = true/' "$CONFIG_FILE"
        echo -e "${GREEN}✓${NC} ClickHouse enabled in configuration" | tee -a "$LOG_FILE"
    fi

    # Step 4: Restart gateways to pick up configuration
    echo -e "\n${BLUE}[4/4]${NC} Restarting gateways (rolling restart)..."

    for i in 1 2 3 4; do
        echo "Restarting gateway_$i..." | tee -a "$LOG_FILE"
        docker-compose -f "$COMPOSE_FILE" restart "gateway_$i"
        sleep 5
        echo -e "${GREEN}✓${NC} gateway_$i restarted" | tee -a "$LOG_FILE"
    done

    echo -e "\n${GREEN}✅ Parallel Operation STARTED${NC}"
    echo "Both NATS and ClickHouse are now active"
}

# Function: Verify parallel operation
verify_parallel() {
    echo -e "\n${BLUE}Verifying Parallel Operation...${NC}"

    CHECKS_PASSED=0
    CHECKS_TOTAL=0

    # Check 1: NATS receiving events
    echo -e "\n${BLUE}Check 1: NATS event flow${NC}"
    CHECKS_TOTAL=$((CHECKS_TOTAL + 1))

    NATS_RATE=$(curl -s 'http://localhost:9090/api/v1/query?query=rate(detector_events_processed_total[5m])' | jq '.data.result[0].value[1]' 2>/dev/null || echo "0")

    if [ "$(echo "$NATS_RATE > 100" | bc)" -eq 1 ]; then
        echo -e "${GREEN}✓${NC} NATS processing at $NATS_RATE events/sec" | tee -a "$LOG_FILE"
        CHECKS_PASSED=$((CHECKS_PASSED + 1))
    else
        echo -e "${YELLOW}⚠${NC} NATS processing low: $NATS_RATE events/sec" | tee -a "$LOG_FILE"
    fi

    # Check 2: ClickHouse receiving events
    echo -e "\n${BLUE}Check 2: ClickHouse event flow${NC}"
    CHECKS_TOTAL=$((CHECKS_TOTAL + 1))

    CH_COUNT=$(docker exec clickhouse clickhouse-client --query "SELECT COUNT() FROM bgp.bgp_stream PREWHERE timestamp > now() - INTERVAL 5 MINUTE;" 2>/dev/null || echo "0")

    if [ "$CH_COUNT" -gt 0 ]; then
        echo -e "${GREEN}✓${NC} ClickHouse receiving events ($CH_COUNT in last 5m)" | tee -a "$LOG_FILE"
        CHECKS_PASSED=$((CHECKS_PASSED + 1))
    else
        echo -e "${YELLOW}⚠${NC} ClickHouse not receiving events" | tee -a "$LOG_FILE"
    fi

    # Check 3: Anomaly detection working
    echo -e "\n${BLUE}Check 3: Anomaly detection${NC}"
    CHECKS_TOTAL=$((CHECKS_TOTAL + 1))

    ANOMALIES=$(curl -s 'http://localhost:9090/api/v1/query?query=rate(detector_anomalies_detected_total[5m])' | jq '.data.result[0].value[1]' 2>/dev/null || echo "0")

    if [ "$(echo "$ANOMALIES > 0" | bc)" -eq 1 ]; then
        echo -e "${GREEN}✓${NC} Anomalies detected: $ANOMALIES/sec" | tee -a "$LOG_FILE"
        CHECKS_PASSED=$((CHECKS_PASSED + 1))
    else
        echo -e "${YELLOW}⚠${NC} No anomalies detected (may be normal)" | tee -a "$LOG_FILE"
    fi

    # Check 4: Error rate
    echo -e "\n${BLUE}Check 4: Error rate${NC}"
    CHECKS_TOTAL=$((CHECKS_TOTAL + 1))

    ERROR_RATE=$(curl -s 'http://localhost:9090/api/v1/query?query=rate(detector_processing_errors_total[5m])' | jq '.data.result[0].value[1]' 2>/dev/null || echo "0")

    if [ "$(echo "$ERROR_RATE < 0.1" | bc)" -eq 1 ]; then
        echo -e "${GREEN}✓${NC} Error rate acceptable: $ERROR_RATE errors/sec" | tee -a "$LOG_FILE"
        CHECKS_PASSED=$((CHECKS_PASSED + 1))
    else
        echo -e "${RED}✗${NC} High error rate: $ERROR_RATE errors/sec" | tee -a "$LOG_FILE"
    fi

    # Summary
    echo -e "\n${BLUE}════════════════════════════════════════════${NC}"
    echo -e "Verification Results: ${GREEN}$CHECKS_PASSED/$CHECKS_TOTAL${NC} checks passed" | tee -a "$LOG_FILE"
    echo -e "${BLUE}════════════════════════════════════════════${NC}"

    if [ "$CHECKS_PASSED" -ge $((CHECKS_TOTAL - 1)) ]; then
        echo -e "\n${GREEN}✅ Parallel Operation VERIFIED${NC}"
        echo "Ready to proceed to Phase 2 (Validation)"
        exit 0
    else
        echo -e "\n${YELLOW}⚠ Some checks failed - investigate before proceeding${NC}"
        exit 1
    fi
}

# Function: Stop parallel operation
stop_parallel() {
    echo -e "\n${RED}Stopping Parallel Operation...${NC}"
    echo -e "${YELLOW}WARNING: This will stop both NATS and ClickHouse processing${NC}"
    read -p "Continue? (yes/no): " -r
    if [[ $REPLY =~ ^[Yy][Ee][Ss]$ ]]; then
        docker-compose -f "$COMPOSE_FILE" stop gateway_1 gateway_2 gateway_3 gateway_4
        echo -e "${GREEN}✓${NC} Gateways stopped"
    else
        echo "Cancelled"
    fi
}

# Main dispatcher
case "$ACTION" in
    start)
        start_parallel
        ;;
    verify)
        verify_parallel
        ;;
    stop)
        stop_parallel
        ;;
    *)
        echo "Usage: $0 [start|verify|stop]"
        echo ""
        echo "  start   - Start parallel operation (NATS + ClickHouse)"
        echo "  verify  - Verify both systems are working"
        echo "  stop    - Stop processing (for maintenance)"
        exit 1
        ;;
esac

echo -e "\n${BLUE}Log saved to: $LOG_FILE${NC}"
