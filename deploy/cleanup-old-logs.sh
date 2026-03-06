#!/bin/bash
# Cleanup Old Logs - Safe Disk Space Recovery
# Safely identifies and removes old log files to free up space
# Usage: ./deploy/cleanup-old-logs.sh [analyze|cleanup|archive|full]

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

ACTION="${1:-analyze}"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
LOG_DIR="/root/log-gateway"
ARCHIVE_DIR="/root/log-gateway-backup/$TIMESTAMP/archived-logs"
DAYS_OLD=${DAYS_OLD:-7}  # Default: logs older than 7 days

echo -e "${BLUE}════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Cleanup Old Logs - Action: $ACTION${NC}"
echo -e "${BLUE}════════════════════════════════════════════${NC}"

# Ensure we're in the right directory
cd /root/log-gateway || exit 1

# Function: Analyze disk usage
analyze_disk() {
    echo -e "\n${BLUE}[1] Current Disk Usage${NC}"
    df -h /root | awk 'NR==1 || NR==2 {print}'

    USAGE=$(df /root | awk 'NR==2 {print $5}' | sed 's/%//')
    AVAILABLE=$(df /root | awk 'NR==2 {print $4}')

    echo -e "\nDisk usage: ${USAGE}%"
    echo -e "Available space: $(numfmt --to=iec $((AVAILABLE * 1024)) 2>/dev/null || echo "$AVAILABLE KB")"

    if [ "$USAGE" -gt 90 ]; then
        echo -e "${RED}✗ CRITICAL: Disk usage > 90% - Immediate cleanup needed${NC}"
    elif [ "$USAGE" -gt 80 ]; then
        echo -e "${YELLOW}⚠ WARNING: Disk usage > 80% - Cleanup recommended${NC}"
    else
        echo -e "${GREEN}✓ Disk usage acceptable${NC}"
    fi

    echo -e "\n${BLUE}[2] Log Directory Sizes${NC}"

    # ClickHouse logs
    if [ -d clickhouse-logs ]; then
        CH_LOG_SIZE=$(du -sh clickhouse-logs | awk '{print $1}')
        CH_LOG_FILES=$(find clickhouse-logs -type f | wc -l)
        echo "ClickHouse logs: $CH_LOG_SIZE ($CH_LOG_FILES files)"
    fi

    # Gateway logs (if any)
    if [ -d data/logs ]; then
        GATEWAY_LOG_SIZE=$(du -sh data/logs | awk '{print $1}')
        GATEWAY_LOG_FILES=$(find data/logs -type f | wc -l)
        echo "Gateway logs: $GATEWAY_LOG_SIZE ($GATEWAY_LOG_FILES files)"
    fi

    # Docker volumes
    if [ -d clickhouse-data ]; then
        CH_DATA_SIZE=$(du -sh clickhouse-data 2>/dev/null | awk '{print $1}' || echo "unknown")
        echo "ClickHouse data: $CH_DATA_SIZE (⚠ DO NOT DELETE)"
    fi

    echo -e "\n${BLUE}[3] Old ClickHouse Log Files (> $DAYS_OLD days)${NC}"

    OLD_LOGS=$(find clickhouse-logs -type f -mtime +$DAYS_OLD 2>/dev/null | wc -l)
    OLD_LOG_SIZE=$(find clickhouse-logs -type f -mtime +$DAYS_OLD 2>/dev/null -exec du -c {} + | tail -1 | awk '{print $1}' | numfmt --to=iec 2>/dev/null || echo "unknown" )

    echo "Found: $OLD_LOGS old log files"
    echo "Space to reclaim: ~$OLD_LOG_SIZE"

    if [ "$OLD_LOGS" -gt 0 ]; then
        echo -e "\n${YELLOW}Sample old files:${NC}"
        find clickhouse-logs -type f -mtime +$DAYS_OLD 2>/dev/null | head -10 | while read -r file; do
            SIZE=$(du -h "$file" | awk '{print $1}')
            MODIFIED=$(stat -f "%Sm" -t "%Y-%m-%d %H:%M:%S" "$file" 2>/dev/null || echo "unknown")
            echo "  $SIZE | $MODIFIED | $(basename "$file")"
        done
    fi
}

# Function: Archive old logs
archive_logs() {
    echo -e "\n${BLUE}[1] Creating archive directory${NC}"
    mkdir -p "$ARCHIVE_DIR"
    echo "Archive location: $ARCHIVE_DIR"

    echo -e "\n${BLUE}[2] Archiving old ClickHouse logs (> $DAYS_OLD days)${NC}"

    OLD_LOG_COUNT=$(find clickhouse-logs -type f -mtime +$DAYS_OLD 2>/dev/null | wc -l)

    if [ "$OLD_LOG_COUNT" -gt 0 ]; then
        find clickhouse-logs -type f -mtime +$DAYS_OLD 2>/dev/null | while read -r file; do
            cp -p "$file" "$ARCHIVE_DIR/" || echo "Warning: Could not copy $file"
        done
        echo -e "${GREEN}✓ Archived $OLD_LOG_COUNT files${NC}"
    else
        echo "No old logs to archive"
    fi

    echo -e "\n${BLUE}[3] Creating compressed archive${NC}"
    ARCHIVE_FILE="/root/log-gateway-backup/$TIMESTAMP/old-logs-$TIMESTAMP.tar.gz"
    if tar -czf "$ARCHIVE_FILE" -C "$ARCHIVE_DIR" . 2>/dev/null; then
        ARCHIVE_SIZE=$(du -h "$ARCHIVE_FILE" | awk '{print $1}')
        echo -e "${GREEN}✓ Created: $ARCHIVE_FILE (${ARCHIVE_SIZE})${NC}"
    else
        echo -e "${YELLOW}⚠ Warning: Could not create tar archive${NC}"
    fi
}

# Function: Cleanup old logs
cleanup_logs() {
    echo -e "\n${RED}════════════════════════════════════════════${NC}"
    echo -e "${RED}  WARNING: This will DELETE old log files${NC}"
    echo -e "${RED}════════════════════════════════════════════${NC}"

    OLD_LOG_COUNT=$(find clickhouse-logs -type f -mtime +$DAYS_OLD 2>/dev/null | wc -l)
    OLD_LOG_SIZE=$(find clickhouse-logs -type f -mtime +$DAYS_OLD 2>/dev/null -exec du -c {} + | tail -1 | awk '{print $1}' | numfmt --to=iec 2>/dev/null || echo "unknown")

    echo -e "\nWill DELETE:"
    echo "  • $OLD_LOG_COUNT log files older than $DAYS_OLD days"
    echo "  • Estimated space to free: $OLD_LOG_SIZE"

    read -p "Archive logs first? (recommended) (yes/no): " -r
    if [[ $REPLY =~ ^[Yy][Ee][Ss]$ ]]; then
        archive_logs
    fi

    echo -e "\n${RED}Are you ABSOLUTELY sure? Type 'DELETE' to confirm:${NC}"
    read -r CONFIRM

    if [ "$CONFIRM" = "DELETE" ]; then
        echo -e "\n${BLUE}Deleting old log files...${NC}"

        DELETED=0
        find clickhouse-logs -type f -mtime +$DAYS_OLD 2>/dev/null | while read -r file; do
            if rm -f "$file"; then
                DELETED=$((DELETED + 1))
                echo "✓ Deleted: $(basename "$file")"
            else
                echo "✗ Failed: $(basename "$file")"
            fi
        done

        echo -e "\n${GREEN}✓ Cleanup complete${NC}"

        echo -e "\n${BLUE}Updated disk usage:${NC}"
        df -h /root | awk 'NR==1 || NR==2 {print}'

        USAGE=$(df /root | awk 'NR==2 {print $5}' | sed 's/%//')
        AVAILABLE=$(df /root | awk 'NR==2 {print $4}')
        echo "Disk usage: ${USAGE}%"
        echo "Available space: $(numfmt --to=iec $((AVAILABLE * 1024)) 2>/dev/null || echo "$AVAILABLE KB")"

    else
        echo -e "${YELLOW}Cancelled - no files deleted${NC}"
        exit 0
    fi
}

# Function: Full cleanup (archive + delete)
full_cleanup() {
    echo -e "\n${BLUE}Performing full cleanup (archive + delete)${NC}"
    analyze_disk
    archive_logs
    cleanup_logs
}

# Main dispatcher
case "$ACTION" in
    analyze)
        analyze_disk
        ;;
    archive)
        archive_logs
        ;;
    cleanup)
        cleanup_logs
        ;;
    full)
        full_cleanup
        ;;
    *)
        echo "Usage: $0 [analyze|archive|cleanup|full]"
        echo ""
        echo "  analyze  - Show disk usage and old log files (no changes)"
        echo "  archive  - Archive old logs to backup directory"
        echo "  cleanup  - Delete old log files (after confirmation)"
        echo "  full     - Archive and delete in one go"
        echo ""
        echo "Environment:"
        echo "  DAYS_OLD - Logs older than N days (default: 7)"
        exit 1
        ;;
esac

echo -e "\n${BLUE}════════════════════════════════════════════${NC}"
