#!/usr/bin/env python3
"""
Binance WebSocket → Log Gateway
================================
Abonniert alle Binance Spot-Trades in Echtzeit (kein API-Key nötig).
Jeder Trade wird als Log-Event ans Gateway geschickt.

Neu: Batch-Modus + HTTP Keep-Alive (persistente TCP-Verbindung pro Worker).

Streams: ~50 wichtigste Handelspaare = 200-500 Events/s

ENV:
  GATEWAY_URL         z.B. http://haproxy-lb:8090
  GATEWAY_API_KEY     Gateway API Key
  GATEWAY_JWT_SECRET  Gateway JWT Secret
  WORKERS             Anzahl HTTP-Sender (default: 4)
  BATCH_SIZE          Events pro Batch (default: 50)
  BATCH_TIMEOUT_MS    Max. Wartezeit auf vollen Batch in ms (default: 50)
  TENANT_ID           (default: binance)
  PAIRS               Komma-getrennt, z.B. btcusdt,ethusdt (default: top 30)
"""

import os, sys, json, time, hmac, hashlib, base64, threading, queue, random, struct
import ssl, socket, http.client
from urllib.request import urlopen, Request
from urllib.parse import urlparse

# ── Konfiguration ─────────────────────────────────────────────────────────────
GATEWAY_URL      = os.environ.get("GATEWAY_URL",        "http://localhost:8090")
API_KEY          = os.environ.get("GATEWAY_API_KEY",    "")
JWT_SECRET       = os.environ.get("GATEWAY_JWT_SECRET", "")
WORKERS          = int(os.environ.get("WORKERS",        "4"))
BATCH_SIZE       = int(os.environ.get("BATCH_SIZE",     "50"))
BATCH_TIMEOUT_MS = int(os.environ.get("BATCH_TIMEOUT_MS", "50"))
TENANT_ID        = os.environ.get("TENANT_ID",          "binance")

DEFAULT_PAIRS = [
    "btcusdt","ethusdt","bnbusdt","solusdt","xrpusdt",
    "adausdt","dogeusdt","avaxusdt","dotusdt","maticusdt",
    "linkusdt","ltcusdt","uniusdt","atomusdt","etcusdt",
    "xlmusdt","vetusdt","filusdt","trxusdt","hbarusdt",
    "nearusdt","algousdt","icpusdt","shibusdt","aaveusdt",
    "axsusdt","sandusdt","manausdt","ftmusdt","grtusdt",
]
PAIRS = [p.lower() for p in os.environ.get("PAIRS", ",".join(DEFAULT_PAIRS)).split(",")]

# ── Gateway Verbindungsparameter ───────────────────────────────────────────────
_parsed  = urlparse(GATEWAY_URL)
GW_HOST  = _parsed.hostname
GW_PORT  = _parsed.port or (443 if _parsed.scheme == "https" else 80)
GW_SCHEME= _parsed.scheme
BATCH_PATH = "/api/v1/logs/batch"

# ── JWT ───────────────────────────────────────────────────────────────────────
def _b64url(s):
    if isinstance(s, str): s = s.encode()
    return base64.urlsafe_b64encode(s).rstrip(b"=").decode()

def make_jwt(secret: str) -> str:
    h = _b64url(json.dumps({"alg":"HS256","typ":"JWT"}))
    p = _b64url(json.dumps({"sub":"binance-stream","tenant_id":TENANT_ID,"exp":9999999999}))
    sig = _b64url(hmac.new(secret.encode(), f"{h}.{p}".encode(), hashlib.sha256).digest())
    return f"{h}.{p}.{sig}"

JWT = make_jwt(JWT_SECRET) if JWT_SECRET else ""

# ── Event-Queue ───────────────────────────────────────────────────────────────
send_queue: queue.Queue = queue.Queue(maxsize=5000)
stats = {"sent": 0, "errors": 0, "batches": 0, "dropped": 0, "start": time.time()}
stats_lock = threading.Lock()

# ── Keep-Alive HTTP Sender ────────────────────────────────────────────────────
def _make_conn():
    if GW_SCHEME == "https":
        ctx = ssl.create_default_context()
        return http.client.HTTPSConnection(GW_HOST, GW_PORT, timeout=10, context=ctx)
    return http.client.HTTPConnection(GW_HOST, GW_PORT, timeout=10)


def sender_worker():
    hdrs = {
        "Content-Type": "application/json",
        "X-Tenant-ID":  TENANT_ID,
        "User-Agent":   "log-gateway-binance/2.0",
        "Connection":   "keep-alive",
    }
    if API_KEY: hdrs["X-API-Key"] = API_KEY
    if JWT:     hdrs["Authorization"] = f"Bearer {JWT}"

    conn = _make_conn()
    batch = []
    deadline = time.monotonic() + BATCH_TIMEOUT_MS / 1000.0

    while True:
        remaining = max(0.001, deadline - time.monotonic())
        try:
            payload = send_queue.get(timeout=min(remaining, BATCH_TIMEOUT_MS / 1000.0))
            batch.append(payload)
            send_queue.task_done()
        except queue.Empty:
            pass

        now = time.monotonic()
        if batch and (len(batch) >= BATCH_SIZE or now >= deadline):
            body = json.dumps(batch).encode("utf-8")
            h = {**hdrs, "Content-Length": str(len(body))}
            try:
                t0 = time.monotonic()
                conn.request("POST", BATCH_PATH, body=body, headers=h)
                resp = conn.getresponse()
                resp.read()
                with stats_lock:
                    stats["sent"]    += len(batch)
                    stats["batches"] += 1
            except Exception:
                try: conn.close()
                except: pass
                conn = _make_conn()
                with stats_lock:
                    stats["errors"] += len(batch)
            batch = []
            deadline = time.monotonic() + BATCH_TIMEOUT_MS / 1000.0


# ── Binance WebSocket ─────────────────────────────────────────────────────────
def process_trade(trade: dict):
    try:
        symbol   = trade.get("s", "UNKNOWN")
        price    = trade.get("p", "0")
        qty      = trade.get("q", "0")
        buyer_mm = trade.get("m", False)
        trade_id = trade.get("t", 0)
        ts       = trade.get("T", int(time.time()*1000))

        side  = "SELL" if buyer_mm else "BUY"
        value = float(price) * float(qty)

        if value > 100_000:
            level = "error"
        elif value > 10_000:
            level = "warn"
        else:
            level = "info"

        payload = {
            "level":   level,
            "message": f"{side} {float(qty):.4f} {symbol} @ ${float(price):,.2f} (${value:,.0f})",
            "source":  "binance-ws",
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ts/1000)),
            "metadata": {
                "symbol": symbol, "price": price, "quantity": qty,
                "side": side, "trade_id": trade_id, "value_usd": round(value, 2),
            }
        }
        try:
            send_queue.put_nowait(payload)
        except queue.Full:
            with stats_lock:
                stats["dropped"] += 1
    except Exception:
        pass


def binance_stream():
    streams = "/".join(f"{p}@trade" for p in PAIRS)
    host, port, path = "stream.binance.com", 9443, f"/stream?streams={streams}"
    ctx = ssl.create_default_context()

    while True:
        try:
            sock  = socket.create_connection((host, port), timeout=30)
            wsock = ctx.wrap_socket(sock, server_hostname=host)

            key = base64.b64encode(random.randbytes(16)).decode()
            wsock.sendall((
                f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            ).encode())

            resp = b""
            while b"\r\n\r\n" not in resp:
                resp += wsock.recv(4096)
            if b"101" not in resp:
                print(f"[error] Handshake fehlgeschlagen: {resp[:200]}", flush=True)
                time.sleep(5); continue

            print("[stream] Binance verbunden ✓ — empfange Trade-Events ...", flush=True)

            buf = bytearray()
            wsock.settimeout(60)

            while True:
                chunk = wsock.recv(65536)
                if not chunk: break
                buf.extend(chunk)

                pos = 0
                while pos + 2 <= len(buf):
                    b0, b1   = buf[pos], buf[pos+1]
                    opcode   = b0 & 0x0f
                    masked   = (b1 & 0x80) != 0
                    plen     = b1 & 0x7f
                    hlen     = pos + 2

                    if plen == 126:
                        if pos + 4 > len(buf): break
                        plen = struct.unpack_from(">H", buf, pos+2)[0]; hlen = pos + 4
                    elif plen == 127:
                        if pos + 10 > len(buf): break
                        plen = struct.unpack_from(">Q", buf, pos+2)[0]; hlen = pos + 10

                    mask_len = 4 if masked else 0
                    end = hlen + mask_len + plen
                    if end > len(buf): break

                    if opcode == 1:
                        data = bytes(buf[hlen + mask_len : end])
                        if masked:
                            mask = buf[hlen:hlen+4]
                            data = bytes(b ^ mask[i%4] for i,b in enumerate(data))
                        try:
                            msg   = json.loads(data)
                            trade = msg.get("data", msg)
                            process_trade(trade)
                        except Exception:
                            pass
                    elif opcode == 8:
                        break
                    elif opcode == 9:
                        wsock.sendall(bytes([0x8a, 0x00]))

                    pos = end

                if pos > 0:
                    del buf[:pos]

        except Exception as e:
            print(f"[stream] {e} — reconnect in 5s ...", flush=True)
            time.sleep(5)


# ── Stats ─────────────────────────────────────────────────────────────────────
def stats_printer():
    while True:
        time.sleep(10)
        elapsed = time.time() - stats["start"]
        with stats_lock:
            s, er, b, dr = stats["sent"], stats["errors"], stats["batches"], stats["dropped"]
        rps = s / elapsed if elapsed > 0 else 0
        bps = b / elapsed if elapsed > 0 else 0
        avg = s / b if b > 0 else 0
        print(f"[stats] sent={s} | errors={er} | dropped={dr} | batches={b} | "
              f"rps={rps:.0f}/s | bps={bps:.2f}/s | avg_batch={avg:.1f} | "
              f"queue={send_queue.qsize()} | elapsed={int(elapsed)}s", flush=True)


# ── Main ──────────────────────────────────────────────────────────────────────
if __name__ == "__main__":
    print("=" * 60)
    print("Binance WebSocket → Log Gateway  (Batch + Keep-Alive)")
    print("=" * 60)
    print(f"  Gateway:    {GATEWAY_URL}{BATCH_PATH}")
    print(f"  Tenant:     {TENANT_ID}")
    print(f"  Workers:    {WORKERS}")
    print(f"  Batch size: {BATCH_SIZE}")
    print(f"  Batch tmo:  {BATCH_TIMEOUT_MS}ms")
    print(f"  Paare:      {len(PAIRS)} ({', '.join(PAIRS[:5])}...)")
    print(f"  API-Key:    {'ja' if API_KEY else 'NEIN ⚠'}")
    print(f"  JWT:        {'ja' if JWT else 'NEIN ⚠'}")
    print("=" * 60, flush=True)

    print("[init] Prüfe Gateway ...", flush=True)
    for attempt in range(10):
        try:
            req = Request(f"{GATEWAY_URL}/health", headers={"User-Agent": "binance-stream/2.0"})
            with urlopen(req, timeout=5) as r: r.read()
            print("[init] Gateway erreichbar ✓", flush=True)
            break
        except Exception as e:
            print(f"[init] Warte auf Gateway ... ({attempt+1}/10): {e}", flush=True)
            time.sleep(3)

    for _ in range(WORKERS):
        threading.Thread(target=sender_worker, daemon=True).start()

    threading.Thread(target=stats_printer, daemon=True).start()

    binance_stream()
