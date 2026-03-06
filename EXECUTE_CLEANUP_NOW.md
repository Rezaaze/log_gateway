# 🚀 EXECUTE CLEANUP NOW - Step by Step

**Target**: Free disk space from 100% to ~80% (15-20GB available)
**Server**: root@167.235.30.106
**Time**: ~5-10 minutes

---

## ⚡ EXECUTE THESE COMMANDS NOW

### Connection
Copy and paste into your terminal:

```bash
ssh root@167.235.30.106
```

**Expected**: You should be logged into the server

---

### Change to correct directory

```bash
cd /root/log-gateway
```

**Expected**: Prompt shows `/root/log-gateway$`

---

### STEP 1: Analyze disk usage (SAFE - no changes)

```bash
./deploy/cleanup-old-logs.sh analyze
```

**Expected output**:
```
[1] Current Disk Usage
Filesystem     Size  Used Avail Use% Mounted on
/dev/sda1       75G   75G    0G  100% /root

Disk usage: 100%
Available space: 0 B

✗ CRITICAL: Disk usage > 90% - Immediate cleanup needed

[2] Log Directory Sizes
ClickHouse logs: 24G (850 files)
ClickHouse data: 48G (⚠ DO NOT DELETE)

[3] Old ClickHouse Log Files (> 7 days)
Found: 500 old log files
Space to reclaim: ~15G
```

**⏱️ Duration**: 30 seconds

**What to do next**: If output shows "CRITICAL" and old log files found, proceed to Step 2

---

### STEP 2: Archive old logs (Create safe backup)

```bash
./deploy/cleanup-old-logs.sh archive
```

**Expected output**:
```
[1] Creating archive directory
Archive location: /root/log-gateway-backup/20260306_010000/archived-logs

[2] Archiving old ClickHouse logs (> 7 days)
✓ Archived 500 files

[3] Creating compressed archive
✓ Created: /root/log-gateway-backup/20260306_010000/old-logs-20260306_010000.tar.gz (2.3G)
```

**⏱️ Duration**: 2-3 minutes (depends on file count)

**What to do next**: Proceed to Step 3

---

### STEP 3: Delete old logs (with confirmation)

```bash
./deploy/cleanup-old-logs.sh cleanup
```

**Script will ask**:
```
Archive logs first? (recommended) (yes/no):
```

**Type**: `no` (we already did it in Step 2)

**Script will then ask**:
```
Are you ABSOLUTELY sure? Type 'DELETE' to confirm:
```

**Type**: `DELETE` (all caps, exactly as shown)

**Expected output after deletion**:
```
✓ Cleanup complete

Updated disk usage:
Filesystem     Size  Used Avail Use% Mounted on
/dev/sda1       75G   60G   15G  80% /root

Disk usage: 80%
Available space: 15G
```

**⏱️ Duration**: 3-5 minutes

**✅ SUCCESS**: Disk now has 15GB available!

---

## ✅ Verification

### Confirm space is freed

```bash
df -h /root
```

**Expected**:
- `Use%` should be ~80% (was 100%)
- `Avail` should be ~15G (was 0)

### Verify ClickHouse data is intact

```bash
docker-compose -f docker-compose.prod.yml exec clickhouse \
  clickhouse-client --query "SELECT COUNT() FROM bgp.bgp_stream;"
```

**Expected**: Large number like `121000000` (not 0)

### Check ClickHouse container is healthy

```bash
docker-compose -f docker-compose.prod.yml ps clickhouse
```

**Expected**: `Status` = `Up (healthy)`

---

## 🚀 NEXT: Phase 0 Migration

Once cleanup is verified, immediately run Phase 0:

```bash
./deploy/phase-0-pre-migration.sh
```

This will:
- ✅ Verify all services running
- ✅ Backup ClickHouse data
- ✅ Document baseline metrics
- ✅ Perform health checks

**Expected output**:
```
✓ ClickHouse backup completed
✓ Baseline metrics saved
✓ Health checks: 5/5 passed
✓ Phase 0 COMPLETE - Ready for Phase 1
```

---

## ⏱️ Timeline

| Step | Action | Duration | Status |
|------|--------|----------|--------|
| 1 | Analyze | 30 sec | ← You are here |
| 2 | Archive | 2-3 min | ← Then here |
| 3 | Delete | 3-5 min | ← Then here |
| ✅ | CLEANUP COMPLETE | ~10 min total | ← Success! |
| 4 | Phase 0 Migration | 1-2 hours | ← After cleanup |

---

## 🆘 If Something Goes Wrong

### "Command not found: ./deploy/cleanup-old-logs.sh"

**Fix**: Make sure you're in correct directory
```bash
pwd  # Should show /root/log-gateway
ls -la deploy/cleanup-old-logs.sh  # Should exist
```

### "Permission denied"

**Fix**: Make file executable
```bash
chmod +x /root/log-gateway/deploy/cleanup-old-logs.sh
./deploy/cleanup-old-logs.sh analyze
```

### "Not enough space to archive"

**Fix**: Skip archiving and delete directly
```bash
./deploy/cleanup-old-logs.sh cleanup
# When asked "Archive logs first?" → say "no"
# When asked to confirm → type "DELETE"
```

### "ClickHouse data shows 0 records"

**⚠️ STOP** - Do not proceed. ClickHouse data may be corrupted.
```bash
# Check ClickHouse logs
docker-compose -f docker-compose.prod.yml logs clickhouse | tail -50
```

---

## 📝 Commands Summary (Copy-Paste)

**All commands in sequence**:

```bash
# 1. Connect
ssh root@167.235.30.106

# 2. Change directory
cd /root/log-gateway

# 3. Analyze (safe)
./deploy/cleanup-old-logs.sh analyze

# 4. Archive
./deploy/cleanup-old-logs.sh archive

# 5. Delete (type: no, then: DELETE)
./deploy/cleanup-old-logs.sh cleanup

# 6. Verify space
df -h /root

# 7. Verify ClickHouse data
docker-compose -f docker-compose.prod.yml exec clickhouse \
  clickhouse-client --query "SELECT COUNT() FROM bgp.bgp_stream;"

# 8. Start Phase 0 migration
./deploy/phase-0-pre-migration.sh
```

---

## ✨ Expected Final State

**After all steps complete**:
- ✅ Disk usage: 80% (15GB available)
- ✅ ClickHouse data: Intact (121M+ events)
- ✅ Old logs: Archived and deleted
- ✅ Phase 0: Complete and successful
- ✅ Ready for Phase 1: Parallel operation

---

## 📞 If You Need Help

1. Show me the output from Step 1 (analyze)
2. Tell me if any step failed
3. Share any error messages

---

**Status**: READY TO EXECUTE
**Next Action**: Run the commands above on production server
