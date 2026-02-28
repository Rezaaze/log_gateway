#!/usr/bin/env python3
"""
Wikipedia Live Stream → Log Gateway
────────────────────────────────────
Abonniert den Wikipedia Recent-Changes SSE-Stream (kostenlos, öffentlich)
und schickt jeden Event als strukturiertes Log-Event ans Gateway.

Neu: Batch-Modus + HTTP Keep-Alive (persistente TCP-Verbindung pro Worker).

Verwendung:
    python3 tools/wikipedia_stream.py

Umgebungsvariablen:
    GATEWAY_URL      Gateway-URL (default: http://localhost:8090)
    GATEWAY_API_KEY  API-Key falls Auth aktiv (default: leer)
    WORKERS          Anzahl paralleler HTTP-Sender (default: 4)
    BATCH_SIZE       Events pro Batch-Request (default: 10)
    BATCH_TIMEOUT_MS Max. Wartezeit auf vollen Batch in ms (default: 50)
    TENANT_ID        Tenant-ID für Rate-Limit-Test (default: wikipedia)
"""

import base64
import hashlib
import hmac
import json
import os
import sys
import time
import threading
import queue
from datetime import datetime, timezone
from urllib.request import urlopen, Request
from urllib.error import URLError, HTTPError
from urllib.parse import urlparse
import http.client

# ── Konfiguration ─────────────────────────────────────────────────────────────
GATEWAY_URL      = os.environ.get("GATEWAY_URL",       "http://localhost:8090")
API_KEY          = os.environ.get("GATEWAY_API_KEY",   "")
JWT_SECRET       = os.environ.get("GATEWAY_JWT_SECRET","")
WORKERS          = int(os.environ.get("WORKERS",       "4"))
BATCH_SIZE       = int(os.environ.get("BATCH_SIZE",    "10"))
BATCH_TIMEOUT_MS = int(os.environ.get("BATCH_TIMEOUT_MS", "50"))
TENANT_ID        = os.environ.get("TENANT_ID",         "wikipedia")


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def make_jwt(secret: str, tenant: str) -> str:
    """Erstellt einen HS256 JWT ohne externe Abhängigkeiten."""
    header  = _b64url(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    now     = int(time.time())
    payload = _b64url(json.dumps({
        "sub": tenant,
        "tenant_id": tenant,
        "iat": now,
        "exp": now + 3600,
    }).encode())
    signing_input = f"{header}.{payload}".encode()
    sig = _b64url(hmac.new(secret.encode(), signing_input, hashlib.sha256).digest())
    return f"{header}.{payload}.{sig}"


_jwt_token   = ""
_jwt_expires = 0
_jwt_lock    = threading.Lock()


def get_jwt() -> str:
    global _jwt_token, _jwt_expires
    if not JWT_SECRET:
        return ""
    with _jwt_lock:
        if time.time() > _jwt_expires - 60:
            _jwt_token   = make_jwt(JWT_SECRET, TENANT_ID)
            _jwt_expires = int(time.time()) + 3600
        return _jwt_token


WIKI_STREAM  = "https://stream.wikimedia.org/v2/stream/recentchange"
_parsed      = urlparse(GATEWAY_URL)
GW_HOST      = _parsed.hostname
GW_PORT      = _parsed.port or (443 if _parsed.scheme == "https" else 80)
GW_SCHEME    = _parsed.scheme
BATCH_PATH   = "/api/v1/logs/batch"
SINGLE_PATH  = "/api/v1/logs"

# ── Statistiken (thread-safe) ─────────────────────────────────────────────────
stats = {"sent": 0, "errors": 0, "batches": 0, "latency": 0.0, "started": time.time()}
stats_lock = threading.Lock()

# ── Event-Queue zwischen Stream-Reader und HTTP-Sendern ───────────────────────
event_queue: queue.Queue = queue.Queue(maxsize=2000)


def parse_sse_event(lines: list) -> dict | None:
    data = None
    for line in lines:
        if line.startswith("data:"):
            data = line[5:].strip()
            break
    if not data:
        return None
    try:
        return json.loads(data)
    except json.JSONDecodeError:
        return None


def stream_wikipedia():
    """Liest den Wikipedia SSE-Stream und füllt die Event-Queue."""
    print(f"[stream] Verbinde mit {WIKI_STREAM} ...", flush=True)
    while True:
        try:
            req = Request(WIKI_STREAM, headers={
                "Accept":     "text/event-stream",
                "User-Agent": "log-gateway-stresstest/1.0",
            })
            with urlopen(req, timeout=30) as resp:
                print("[stream] Verbunden — empfange Events ...", flush=True)
                buffer = []
                for raw_line in resp:
                    line = raw_line.decode("utf-8").rstrip("\n").rstrip("\r")
                    if line == "":
                        if buffer:
                            event = parse_sse_event(buffer)
                            if event:
                                try:
                                    event_queue.put_nowait(event)
                                except queue.Full:
                                    pass
                            buffer = []
                    else:
                        buffer.append(line)
        except (URLError, OSError) as e:
            print(f"[stream] Verbindungsfehler: {e} — reconnect in 5s ...", flush=True)
            time.sleep(5)


def build_log_payload(event: dict) -> dict:
    return {
        "tenant_id":  TENANT_ID,
        "level":      "info",
        "message":    f"[{event.get('type', 'edit')}] {event.get('title', '')} by {event.get('user', 'anon')}",
        "source":     event.get("server_name", "wikipedia"),
        "service":    event.get("wiki", "wikipedia"),
        "timestamp":  datetime.now(timezone.utc).isoformat(),
        "metadata": {
            "wiki":        event.get("wiki", ""),
            "title":       event.get("title", ""),
            "user":        event.get("user", ""),
            "change_type": event.get("type", ""),
            "server":      event.get("server_name", ""),
            "namespace":   str(event.get("namespace", 0)),
            "bot":         str(event.get("bot", False)),
        }
    }


def _make_conn():
    """Erstellt eine neue persistente HTTP-Verbindung zum Gateway."""
    if GW_SCHEME == "https":
        import ssl
        ctx = ssl.create_default_context()
        return http.client.HTTPSConnection(GW_HOST, GW_PORT, timeout=10, context=ctx)
    return http.client.HTTPConnection(GW_HOST, GW_PORT, timeout=10)


def _send_batch(conn, batch: list, hdrs: dict):
    """Sendet einen Batch über die bestehende Keep-Alive-Verbindung.
    Gibt (conn, ok) zurück — conn kann None sein wenn die Verbindung abgebrochen ist."""
    body = json.dumps(batch).encode("utf-8")
    hdrs_with_len = {**hdrs, "Content-Length": str(len(body))}
    try:
        t0 = time.monotonic()
        conn.request("POST", BATCH_PATH, body=body, headers=hdrs_with_len)
        resp = conn.getresponse()
        resp.read()  # Antwort leeren damit die Verbindung wiederverwendet werden kann
        latency = time.monotonic() - t0
        with stats_lock:
            stats["sent"]    += len(batch)
            stats["batches"] += 1
            stats["latency"] += latency
        return conn, True
    except Exception:
        # Verbindung tot — neu aufbauen
        try: conn.close()
        except: pass
        return None, False


def worker(worker_id: int):
    """Worker: sammelt Events in Batch-Buffer, sendet mit Keep-Alive."""
    hdrs = {
        "Content-Type": "application/json",
        "X-Tenant-ID":  TENANT_ID,
        "User-Agent":   "log-gateway-wikipedia/2.0",
        "Connection":   "keep-alive",
    }
    if API_KEY:
        hdrs["X-API-Key"] = API_KEY
    jwt = get_jwt()
    if jwt:
        hdrs["Authorization"] = f"Bearer {jwt}"

    conn = _make_conn()
    batch = []
    deadline = time.monotonic() + BATCH_TIMEOUT_MS / 1000.0

    while True:
        # Wie lange noch bis Timeout?
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            remaining = 0.001

        try:
            event = event_queue.get(timeout=min(remaining, BATCH_TIMEOUT_MS / 1000.0))
            batch.append(build_log_payload(event))
            event_queue.task_done()
        except queue.Empty:
            pass

        # Batch senden wenn voll oder Timeout
        now = time.monotonic()
        if batch and (len(batch) >= BATCH_SIZE or now >= deadline):
            if conn is None:
                conn = _make_conn()
            conn, ok = _send_batch(conn, batch, hdrs)
            if not ok:
                with stats_lock:
                    stats["errors"] += len(batch)
                conn = _make_conn()
            batch = []
            deadline = time.monotonic() + BATCH_TIMEOUT_MS / 1000.0

            # JWT erneuern falls nötig
            jwt = get_jwt()
            if jwt:
                hdrs["Authorization"] = f"Bearer {jwt}"


def stats_printer():
    while True:
        time.sleep(10)
        with stats_lock:
            elapsed  = time.time() - stats["started"]
            sent     = stats["sent"]
            errors   = stats["errors"]
            batches  = stats["batches"]
            avg_lat  = (stats["latency"] / batches * 1000) if batches > 0 else 0
            q_size   = event_queue.qsize()
        rps = sent / elapsed if elapsed > 0 else 0
        bps = batches / elapsed if elapsed > 0 else 0
        print(
            f"[stats] sent={sent} | errors={errors} | batches={batches} | "
            f"rps={rps:.1f}/s | bps={bps:.2f}/s | "
            f"avg_batch_latency={avg_lat:.1f}ms | queue={q_size} | elapsed={elapsed:.0f}s",
            flush=True
        )


def main():
    print("=" * 60)
    print("Wikipedia SSE → Log Gateway  (Batch + Keep-Alive)")
    print("=" * 60)
    print(f"  Gateway:     {GATEWAY_URL}{BATCH_PATH}")
    print(f"  Tenant:      {TENANT_ID}")
    print(f"  Workers:     {WORKERS}")
    print(f"  Batch size:  {BATCH_SIZE}")
    print(f"  Batch tmo:   {BATCH_TIMEOUT_MS}ms")
    print(f"  API-Key:     {'ja' if API_KEY else 'nein'}")
    print(f"  JWT:         {'ja' if JWT_SECRET else 'nein'}")
    print("=" * 60, flush=True)

    try:
        req = Request(f"{GATEWAY_URL}/health")
        with urlopen(req, timeout=3) as resp:
            resp.read()
        print("[init] Gateway erreichbar ✓", flush=True)
    except Exception as e:
        print(f"[init] WARNUNG: Gateway nicht erreichbar: {e}", flush=True)

    for i in range(WORKERS):
        threading.Thread(target=worker, args=(i,), daemon=True).start()

    threading.Thread(target=stats_printer, daemon=True).start()

    try:
        stream_wikipedia()
    except KeyboardInterrupt:
        with stats_lock:
            sent   = stats["sent"]
            errors = stats["errors"]
        print(f"\n[stop] Beendet — {sent} Events gesendet, {errors} Fehler")
        sys.exit(0)


if __name__ == "__main__":
    main()
