#!/usr/bin/env python3
"""
Wikipedia Live Stream → Log Gateway
────────────────────────────────────
Abonniert den Wikipedia Recent-Changes SSE-Stream (kostenlos, öffentlich)
und schickt jeden Event als strukturiertes Log-Event ans Gateway.

Verwendung:
    python3 tools/wikipedia_stream.py

Umgebungsvariablen:
    GATEWAY_URL      Gateway-URL (default: http://localhost:8090)
    GATEWAY_API_KEY  API-Key falls Auth aktiv (default: leer)
    WORKERS          Anzahl paralleler HTTP-Sender (default: 4)
    BATCH_SIZE       Events pro Batch-Request (default: 1)
    TENANT_ID        Tenant-ID für Rate-Limit-Test (default: wikipedia)
"""

import json
import os
import sys
import time
import threading
import queue
from datetime import datetime, timezone
from urllib.request import urlopen, Request
from urllib.error import URLError, HTTPError

# ── Konfiguration ─────────────────────────────────────────────────────────────
GATEWAY_URL  = os.environ.get("GATEWAY_URL",     "http://localhost:8090")
API_KEY      = os.environ.get("GATEWAY_API_KEY", "")
WORKERS      = int(os.environ.get("WORKERS",     "4"))
BATCH_SIZE   = int(os.environ.get("BATCH_SIZE",  "1"))
TENANT_ID    = os.environ.get("TENANT_ID",       "wikipedia")

WIKI_STREAM  = "https://stream.wikimedia.org/v2/stream/recentchange"
INGEST_URL   = f"{GATEWAY_URL}/api/v1/logs"

# ── Statistiken (thread-safe) ─────────────────────────────────────────────────
stats = {
    "sent":    0,
    "errors":  0,
    "latency": 0.0,
    "started": time.time(),
}
stats_lock = threading.Lock()

# ── Event-Queue zwischen Stream-Reader und HTTP-Sendern ───────────────────────
event_queue: queue.Queue = queue.Queue(maxsize=1000)


def parse_sse_event(lines: list[str]) -> dict | None:
    """Parst einen SSE-Block (mehrere Zeilen) zu einem Dict."""
    data = None
    for line in lines:
        if line.startswith("data:"):
            data = line[5:].strip()
            break
    if not data or data == "":
        return None
    try:
        return json.loads(data)
    except json.JSONDecodeError:
        return None


def stream_wikipedia():
    """Liest den Wikipedia SSE-Stream und füllt die Event-Queue."""
    print(f"[stream] Verbinde mit {WIKI_STREAM} ...")
    while True:
        try:
            req = Request(WIKI_STREAM, headers={"Accept": "text/event-stream"})
            with urlopen(req, timeout=30) as resp:
                print("[stream] Verbunden — empfange Events ...")
                buffer = []
                for raw_line in resp:
                    line = raw_line.decode("utf-8").rstrip("\n").rstrip("\r")
                    if line == "":
                        # Leere Zeile = Ende eines SSE-Events
                        if buffer:
                            event = parse_sse_event(buffer)
                            if event:
                                try:
                                    event_queue.put(event, timeout=1)
                                except queue.Full:
                                    pass  # Queue voll → Event verwerfen
                            buffer = []
                    else:
                        buffer.append(line)
        except (URLError, OSError) as e:
            print(f"[stream] Verbindungsfehler: {e} — reconnect in 5s ...")
            time.sleep(5)


def build_log_payload(event: dict) -> dict:
    """Wandelt ein Wikipedia-Event in ein Gateway-kompatibles Log-Payload um."""
    # Wikipedia Event-Felder: type, title, user, wiki, server_name, timestamp, ...
    return {
        "tenant_id":  TENANT_ID,
        "level":      "INFO",
        "message":    f"[{event.get('type', 'edit')}] {event.get('title', '')} by {event.get('user', 'anon')}",
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


def send_to_gateway(payload: dict) -> bool:
    """Sendet ein einzelnes Log-Payload ans Gateway. Gibt True bei Erfolg zurück."""
    body = json.dumps(payload).encode("utf-8")
    headers = {
        "Content-Type": "application/json",
        "Content-Length": str(len(body)),
    }
    if API_KEY:
        headers["X-API-Key"] = API_KEY

    try:
        t0 = time.monotonic()
        req = Request(INGEST_URL, data=body, headers=headers, method="POST")
        with urlopen(req, timeout=5) as resp:
            resp.read()
            latency = time.monotonic() - t0
            with stats_lock:
                stats["sent"]    += 1
                stats["latency"] += latency
            return True
    except HTTPError as e:
        with stats_lock:
            stats["errors"] += 1
        if e.code == 429:
            time.sleep(0.1)  # Rate-limit → kurz warten
        return False
    except (URLError, OSError):
        with stats_lock:
            stats["errors"] += 1
        return False


def worker(worker_id: int):
    """Worker-Thread: nimmt Events aus der Queue und schickt sie ans Gateway."""
    while True:
        try:
            event = event_queue.get(timeout=5)
            payload = build_log_payload(event)
            send_to_gateway(payload)
            event_queue.task_done()
        except queue.Empty:
            continue


def stats_printer():
    """Gibt alle 10 Sekunden Statistiken aus."""
    while True:
        time.sleep(10)
        with stats_lock:
            elapsed  = time.time() - stats["started"]
            sent     = stats["sent"]
            errors   = stats["errors"]
            total    = sent + errors
            rps      = sent / elapsed if elapsed > 0 else 0
            avg_lat  = (stats["latency"] / sent * 1000) if sent > 0 else 0
            q_size   = event_queue.qsize()

        print(
            f"[stats] "
            f"sent={sent} | errors={errors} | "
            f"rps={rps:.1f}/s | "
            f"avg_latency={avg_lat:.1f}ms | "
            f"queue={q_size} | "
            f"elapsed={elapsed:.0f}s"
        )


def main():
    print("=" * 60)
    print("Wikipedia SSE → Log Gateway")
    print("=" * 60)
    print(f"  Gateway:   {INGEST_URL}")
    print(f"  Tenant:    {TENANT_ID}")
    print(f"  Workers:   {WORKERS}")
    print(f"  Auth:      {'ja' if API_KEY else 'nein'}")
    print("=" * 60)

    # Kurzer Verbindungstest
    try:
        req = Request(f"{GATEWAY_URL}/health")
        with urlopen(req, timeout=3) as resp:
            resp.read()
        print(f"[init] Gateway erreichbar ✓")
    except Exception as e:
        print(f"[init] WARNUNG: Gateway nicht erreichbar: {e}")
        print(f"[init] Starte trotzdem — Events werden gepuffert ...")

    # Worker-Threads starten
    for i in range(WORKERS):
        t = threading.Thread(target=worker, args=(i,), daemon=True)
        t.start()

    # Stats-Printer starten
    t = threading.Thread(target=stats_printer, daemon=True)
    t.start()

    # Stream-Reader im Hauptthread (blockiert)
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
