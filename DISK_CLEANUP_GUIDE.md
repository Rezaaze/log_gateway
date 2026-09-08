# 🔧 Disk Cleanup Guide - Free Up Space for Migration

> **⚠️ VERALTET (Stand 08.09.2026).** Dieses Dokument beschreibt Arbeiten an
> einem Server, der stillgelegt wurde und nicht mehr existiert
> (167.235.30.106). Die beschriebenen Schritte nicht mehr ausführen. Das
> Dokument bleibt als Protokoll erhalten; den aktuellen Stand beschreibt
> `CLAUDE.md`.

**Current Status**: Disk 100% full (75GB/75GB) - CRITICAL ⚠️
**Requirement**: Need > 20GB free before Phase 0 migration

---

## 📊 Quick Diagnosis

```bash
# Check current disk usage
df -h /root

# Expected output:
# Filesystem     Size  Used Avail Use% Mounted on
# /dev/sda1      75G   75G    0G  100% /root
```

If you see `100%` in the `Use%` column, follow this guide to free up space.

---

## 🚀 Safe Cleanup Process (3 Steps)

### Step 1: Analyze What's Taking Space

```bash
ssh root@167.235.30.106
cd /root/log-gateway

# Analyze disk usage (no changes made)
./deploy/cleanup-old-logs.sh analyze
```

**Expected output**:
```
✓ CRITICAL: Disk usage > 90% - Immediate cleanup needed

[2] Log Directory Sizes
ClickHouse logs: 24G (850 files)
ClickHouse data: 48G (⚠ DO NOT DELETE)

[3] Old ClickHouse Log Files (> 7 days)
Found: 500 old log files
Space to reclaim: ~15G
```

### Step 2: Archive Old Logs (Safe Backup)

```bash
# Archive old logs before deleting
./deploy/cleanup-old-logs.sh archive
```

**What it does**:
- ✅ Copies old logs to `/root/log-gateway-backup/<timestamp>/archived-logs/`
- ✅ Creates compressed tar.gz archive
- ✅ Preserves logs for future reference
- ✅ No data loss

**Expected output**:
```
✓ Archived 500 files
✓ Created: /root/log-gateway-backup/20260306_010000/old-logs-20260306_010000.tar.gz (2.3G)
```

### Step 3: Delete Old Log Files

```bash
# Delete old logs (after confirmation)
./deploy/cleanup-old-logs.sh cleanup
```

**Safety prompts**:
1. Asks to archive first (recommend "yes")
2. Shows what will be deleted
3. Requires you to type "DELETE" to confirm

**Expected output**:
```
Will DELETE:
  • 500 log files older than 7 days
  • Estimated space to free: ~15G

Are you ABSOLUTELY sure? Type 'DELETE' to confirm:
> DELETE

✓ Cleanup complete

Updated disk usage:
Filesystem     Size  Used Avail Use% Mounted on
/dev/sda1      75G   60G   15G  80% /root
```

---

## 🎯 All-In-One Command

If you want to do everything in one go:

```bash
./deploy/cleanup-old-logs.sh full
```

This will:
1. Analyze disk usage
2. Archive old logs
3. Delete old logs (with confirmation)
4. Show updated disk usage

---

## ⚙️ Script Options

```bash
# Analyze only (safe - no changes)
./deploy/cleanup-old-logs.sh analyze

# Archive old logs only (create backup)
./deploy/cleanup-old-logs.sh archive

# Delete old logs only (need confirmation)
./deploy/cleanup-old-logs.sh cleanup

# Everything together
./deploy/cleanup-old-logs.sh full
```

### Environment Variables

```bash
# Delete logs older than 14 days instead of default 7 days
DAYS_OLD=14 ./deploy/cleanup-old-logs.sh full

# Delete logs older than 30 days
DAYS_OLD=30 ./deploy/cleanup-old-logs.sh cleanup
```

---

## 📋 What Gets Deleted

### ✅ SAFE TO DELETE
- Old ClickHouse log files (`.log` files older than 7 days)
- Gateway application logs (if any)
- Temporary log files

### ❌ DO NOT DELETE
- **ClickHouse data** (`/root/log-gateway/clickhouse-data`) - Contains actual BGP events!
- **NATS data** (`/root/log-gateway/nats-data`)
- **Prometheus data** (`/root/log-gateway/prometheus-data`)
- **Config files**

The script is designed to ONLY delete log files, never data directories.

---

## 🆘 If Something Goes Wrong

### Problem: Script fails to delete files

**Solution**:
```bash
# Check permissions
ls -l /root/log-gateway/clickhouse-logs/

# If files are owned by different user, may need:
sudo chown -R root:root /root/log-gateway/clickhouse-logs/
```

### Problem: Not enough space freed

**Solution**: Delete older logs

```bash
# Try deleting logs older than 3 days
DAYS_OLD=3 ./deploy/cleanup-old-logs.sh cleanup
```

### Problem: Accidental deletion

**Solution**: Archives are backed up
```bash
# Check what was archived
ls -lh /root/log-gateway-backup/*/old-logs-*.tar.gz

# Restore if needed
cd /root/log-gateway-backup/<timestamp>/
tar -xzf old-logs-*.tar.gz -C /root/log-gateway/clickhouse-logs/
```

---

## 📈 Target Disk Usage

### Current State
```
Filesystem: 75G total, 75G used, 0G available (100% FULL)
```

### After Cleanup Target
```
Filesystem: 75G total, 60G used, 15G available (80% used)
```

### Minimum Required for Phase 0
```
Filesystem: 75G total, 55G used, 20G available (73% used)
```

---

## ✅ Verification Steps

### Step 1: Confirm space is freed
```bash
df -h /root
```

**Expected**: At least 15-20GB available (should show > 80% or less)

### Step 2: Verify ClickHouse data is intact
```bash
docker-compose -f docker-compose.prod.yml exec clickhouse \
  clickhouse-client --query "SELECT COUNT() FROM bgp.bgp_stream;"
```

**Expected**: Large number (e.g., 121000000), not 0

### Step 3: Check ClickHouse container health
```bash
docker-compose -f docker-compose.prod.yml ps clickhouse
```

**Expected**: Status = "Up (healthy)"

---

## 🔄 Cleanup Sequence

**Recommended order**:

1. ✅ Run `analyze` to see disk usage
2. ✅ Run `archive` to back up old logs
3. ✅ Run `cleanup` to delete logs (with confirmation)
4. ✅ Verify with `df -h` that space is freed
5. ✅ Proceed to Phase 0

---

## 📝 Example: Full Cleanup Session

```bash
# SSH into production server
ssh root@167.235.30.106

# Change to correct directory
cd /root/log-gateway

# Step 1: Analyze
./deploy/cleanup-old-logs.sh analyze
# Output shows 24GB in ClickHouse logs, 500 old files

# Step 2: Archive (safe backup)
./deploy/cleanup-old-logs.sh archive
# Output shows logs archived to backup directory

# Step 3: Delete (with confirmation)
./deploy/cleanup-old-logs.sh cleanup
# Asks to archive (say "yes" if not done)
# Shows what will be deleted
# Requires typing "DELETE" to confirm
# Deletes files and shows updated disk usage

# Step 4: Verify
df -h /root
# Should show 15-20GB available now

# Ready for Phase 0!
```

---

## 🎉 Next Steps After Cleanup

Once disk space is freed (> 20GB available):

1. ✅ Disk cleanup complete
2. ✅ Run Phase 0 migration:
   ```bash
   ./deploy/phase-0-pre-migration.sh
   ```

3. ✅ If Phase 0 passes, proceed to Phase 1:
   ```bash
   ./deploy/phase-1-parallel-operation.sh start
   ```

---

## 📞 Support

If cleanup fails:

1. Check logs: `docker-compose -f docker-compose.prod.yml logs clickhouse`
2. Verify ClickHouse is running
3. Ensure enough disk space to create archives
4. Try with smaller DAYS_OLD value

For other issues, see: `docs/TROUBLESHOOTING.md`

---

**Status**: Script ready ✅ - Execute on production server when disk is full
