#!/usr/bin/env python3
"""
BGP Stream (RIPE NCC RIS Live) → Log Gateway
Kein API-Key nötig. ~1.000-6.000 Events/s vom globalen Internet-Routing.

Neu: Batch-Modus + HTTP Keep-Alive (persistente TCP-Verbindung pro Worker).
"""
import os, json, time, hmac, hashlib, base64, threading, queue, ssl, socket, struct, random
import http.client
from urllib.request import urlopen, Request
from urllib.parse import urlparse

GATEWAY_URL      = os.environ.get("GATEWAY_URL",        "http://localhost:8090")
API_KEY          = os.environ.get("GATEWAY_API_KEY",    "")
JWT_SECRET       = os.environ.get("GATEWAY_JWT_SECRET", "")
WORKERS          = int(os.environ.get("WORKERS",        "8"))
BATCH_SIZE       = int(os.environ.get("BATCH_SIZE",     "100"))
BATCH_TIMEOUT_MS = int(os.environ.get("BATCH_TIMEOUT_MS", "50"))
TENANT_ID        = os.environ.get("TENANT_ID",          "bgp")
SAMPLE_RATE      = float(os.environ.get("SAMPLE_RATE",  "0.2"))  # 20% der Events senden

_parsed  = urlparse(GATEWAY_URL)
GW_HOST  = _parsed.hostname
GW_PORT  = _parsed.port or (443 if _parsed.scheme == "https" else 80)
GW_SCHEME= _parsed.scheme
BATCH_PATH = "/api/v1/logs/batch"

def _b64url(s):
    if isinstance(s, str): s = s.encode()
    return base64.urlsafe_b64encode(s).rstrip(b"=").decode()

def make_jwt(secret):
    h = _b64url('{"alg":"HS256","typ":"JWT"}')
    p = _b64url(f'{{"sub":"bgp","tenant_id":"{TENANT_ID}","exp":9999999999}}')
    sig = _b64url(hmac.new(secret.encode(), f"{h}.{p}".encode(), hashlib.sha256).digest())
    return f"{h}.{p}.{sig}"

JWT = make_jwt(JWT_SECRET) if JWT_SECRET else ""

send_queue = queue.Queue(maxsize=10000)
stats = {"sent":0,"errors":0,"batches":0,"dropped":0,"ann":0,"with":0,"start":time.time()}
lock = threading.Lock()

KNOWN_AS = {
    "15169":"Google","8075":"Microsoft","16509":"Amazon AWS",
    "13335":"Cloudflare","32934":"Meta","714":"Apple",
    "2906":"Netflix","20940":"Akamai","6939":"Hurricane Electric",
    "1299":"Telia","3356":"Lumen","174":"Cogent","3320":"Deutsche Telekom",
    "2914":"NTT","7018":"AT&T","4134":"China Telecom","7922":"Comcast",
}

# ── Keep-Alive HTTP Sender ────────────────────────────────────────────────────
def _make_conn():
    if GW_SCHEME == "https":
        ctx = ssl.create_default_context()
        return http.client.HTTPSConnection(GW_HOST, GW_PORT, timeout=10, context=ctx)
    return http.client.HTTPConnection(GW_HOST, GW_PORT, timeout=10)


def sender():
    hdrs = {
        "Content-Type": "application/json",
        "X-Tenant-ID":  TENANT_ID,
        "User-Agent":   "bgp-stream/2.0",
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
            pl = send_queue.get(timeout=min(remaining, BATCH_TIMEOUT_MS / 1000.0))
            batch.append(pl)
            send_queue.task_done()
        except queue.Empty:
            pass

        now = time.monotonic()
        if batch and (len(batch) >= BATCH_SIZE or now >= deadline):
            body = json.dumps(batch).encode("utf-8")
            h = {**hdrs, "Content-Length": str(len(body))}
            try:
                conn.request("POST", BATCH_PATH, body=body, headers=h)
                resp = conn.getresponse()
                resp.read()
                with lock:
                    stats["sent"]    += len(batch)
                    stats["batches"] += 1
            except Exception:
                try: conn.close()
                except: pass
                conn = _make_conn()
                with lock:
                    stats["errors"] += len(batch)
            batch = []
            deadline = time.monotonic() + BATCH_TIMEOUT_MS / 1000.0


def enqueue(event, prefix, peer_asn, origin_asn, path, nexthop, ts):
    # Sampling: nur SAMPLE_RATE% der Events senden
    if random.random() > SAMPLE_RATE:
        return

    origin = KNOWN_AS.get(str(origin_asn), f"AS{origin_asn}")
    peer   = KNOWN_AS.get(str(peer_asn),   f"AS{peer_asn}")
    level  = "warn" if event == "WITHDRAW" else "info"
    path_s = " → ".join(str(a) for a in (path or [])[-4:]) or str(peer_asn)
    msg    = (f"ANNOUNCE {prefix} via {peer} (path: {path_s})"
              if event == "ANNOUNCE"
              else f"WITHDRAW {prefix} von {peer}")
    try:
        ts_s = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(float(ts)))
    except Exception:
        ts_s = time.strftime("%Y-%m-%dT%H:%M:%SZ")

    pl = {
        "level": level,
        "message": msg,
        "source": "ripe-ris",
        "timestamp": ts_s,
        "metadata": {
            "event": event, "prefix": prefix, "peer_asn": str(peer_asn),
            "origin_asn": str(origin_asn), "origin": origin, "nexthop": nexthop,
        }
    }
    try:
        send_queue.put_nowait(pl)
    except queue.Full:
        with lock: stats["dropped"] += 1


def process(data):
    if not isinstance(data, dict): return
    ts       = data.get("timestamp", time.time())
    peer_asn = data.get("peer_asn", "0")
    path     = data.get("path", [])
    origin   = path[-1] if path else peer_asn

    for ann in data.get("announcements", []):
        nh = ann.get("next_hop","")
        for pfx in ann.get("prefixes",[]):
            enqueue("ANNOUNCE", pfx, peer_asn, origin, path, nh, ts)
            with lock: stats["ann"] += 1

    for pfx in data.get("withdrawals", []):
        enqueue("WITHDRAW", pfx, peer_asn, origin, path, "", ts)
        with lock: stats["with"] += 1


def bgp_stream():
    host, port, path = "ris-live.ripe.net", 443, "/v1/ws/"
    ctx = ssl.create_default_context()
    sub = json.dumps({"type":"ris_subscribe","data":{"type":"UPDATE"}}).encode()

    while True:
        try:
            raw = socket.create_connection((host, port), timeout=30)
            ws  = ctx.wrap_socket(raw, server_hostname=host)

            key = base64.b64encode(random.randbytes(16)).decode()
            ws.sendall((
                f"GET {path} HTTP/1.1\r\nHost: {host}\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
                "User-Agent: bgp-stream/2.0\r\n\r\n"
            ).encode())
            resp = b""
            while b"\r\n\r\n" not in resp:
                resp += ws.recv(4096)
            if b"101" not in resp:
                raise ConnectionError("Handshake failed")

            print("[stream] BGP verbunden ✓", flush=True)

            mask = random.randbytes(4)
            masked = bytes(b ^ mask[i%4] for i,b in enumerate(sub))
            ws.sendall(bytes([0x81, 0x80|len(sub)]) + mask + masked)
            print("[stream] Abonniert — empfange BGP Updates ...", flush=True)

            buf = bytearray()
            ws.settimeout(60)

            while True:
                chunk = ws.recv(131072)
                if not chunk: break
                buf.extend(chunk)

                pos = 0
                while pos + 2 <= len(buf):
                    b0 = buf[pos]; b1 = buf[pos+1]
                    opcode = b0 & 0x0f
                    plen   = b1 & 0x7f
                    hlen   = pos + 2

                    if plen == 126:
                        if pos + 4 > len(buf): break
                        plen = struct.unpack_from(">H", buf, pos+2)[0]; hlen = pos + 4
                    elif plen == 127:
                        if pos + 10 > len(buf): break
                        plen = struct.unpack_from(">Q", buf, pos+2)[0]; hlen = pos + 10

                    end = hlen + plen
                    if end > len(buf): break

                    if opcode == 1:
                        try:
                            msg = json.loads(buf[hlen:end])
                            if msg.get("type") == "ris_message":
                                process(msg.get("data", {}))
                        except Exception:
                            pass
                    elif opcode == 8:
                        print("[stream] Close frame", flush=True)
                        pos = end; break
                    elif opcode == 9:
                        ws.sendall(bytes([0x8a, 0x00]))

                    pos = end

                if pos > 0:
                    del buf[:pos]

        except Exception as e:
            print(f"[stream] {e} — reconnect in 5s ...", flush=True)
            time.sleep(5)


def printer():
    while True:
        time.sleep(10)
        e = time.time() - stats["start"]
        with lock:
            s,er,b,dr,a,w = stats["sent"],stats["errors"],stats["batches"],stats["dropped"],stats["ann"],stats["with"]
        rps = s/e if e > 0 else 0
        bps = b/e if e > 0 else 0
        avg = s/b if b > 0 else 0
        print(f"[stats] sent={s} | errors={er} | dropped={dr} | batches={b} | "
              f"rps={rps:.0f}/s | bps={bps:.2f}/s | avg_batch={avg:.1f} | "
              f"queue={send_queue.qsize()} | ann={a} | with={w} | elapsed={int(e)}s", flush=True)


if __name__ == "__main__":
    print("="*60)
    print("BGP Stream (RIPE NCC RIS Live) → Log Gateway  (Batch + Keep-Alive)")
    print("="*60)
    print(f"  Gateway:     {GATEWAY_URL}{BATCH_PATH}")
    print(f"  Workers:     {WORKERS}")
    print(f"  Batch size:  {BATCH_SIZE}")
    print(f"  Batch tmo:   {BATCH_TIMEOUT_MS}ms")
    print(f"  Sample rate: {SAMPLE_RATE*100:.0f}%")
    print(f"  API-Key:     {'ja' if API_KEY else 'NEIN'}")
    print(f"  JWT:         {'ja' if JWT else 'NEIN'}")
    print("="*60, flush=True)

    for _ in range(WORKERS):
        threading.Thread(target=sender, daemon=True).start()
    threading.Thread(target=printer, daemon=True).start()

    bgp_stream()
