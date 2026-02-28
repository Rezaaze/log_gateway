#!/usr/bin/env python3
"""
BGP Stream (RIPE NCC RIS Live) → Log Gateway
Kein API-Key nötig. ~10.000-15.000 Events/s vom globalen Internet-Routing.

Architektur v3:
- Jeder Worker hat seine eigene private Queue (kein GIL-Contention auf shared Queue)
- Round-Robin Verteilung der Events auf Worker-Queues
- Batch-100 + HTTP Keep-Alive pro Worker
- Ziel: 10.000+ events/s ohne Drop
"""
import os, json, time, hmac, hashlib, base64, threading, queue, ssl, socket, struct, random
import http.client
from urllib.parse import urlparse

GATEWAY_URL      = os.environ.get("GATEWAY_URL",        "http://localhost:8090")
API_KEY          = os.environ.get("GATEWAY_API_KEY",    "")
JWT_SECRET       = os.environ.get("GATEWAY_JWT_SECRET", "")
WORKERS          = int(os.environ.get("WORKERS",        "16"))
BATCH_SIZE       = int(os.environ.get("BATCH_SIZE",     "100"))
BATCH_TIMEOUT_MS = int(os.environ.get("BATCH_TIMEOUT_MS", "20"))
TENANT_ID        = os.environ.get("TENANT_ID",          "bgp")
SAMPLE_RATE      = float(os.environ.get("SAMPLE_RATE",  "1.0"))
QUEUE_PER_WORKER = int(os.environ.get("QUEUE_PER_WORKER", "2000"))

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

# Jeder Worker bekommt seine eigene Queue
worker_queues = [queue.Queue(maxsize=QUEUE_PER_WORKER) for _ in range(WORKERS)]
_rr_counter = 0
_rr_lock = threading.Lock()

stats = {"sent":0,"errors":0,"dropped":0,"ann":0,"with":0,"start":time.time()}
_stats_lock = threading.Lock()

KNOWN_AS = {
    "15169":"Google","8075":"Microsoft","16509":"Amazon AWS",
    "13335":"Cloudflare","32934":"Meta","714":"Apple",
    "2906":"Netflix","20940":"Akamai","6939":"Hurricane Electric",
    "1299":"Telia","3356":"Lumen","174":"Cogent","3320":"Deutsche Telekom",
    "2914":"NTT","7018":"AT&T","4134":"China Telecom","7922":"Comcast",
}

def _make_conn():
    if GW_SCHEME == "https":
        ctx = ssl.create_default_context()
        return http.client.HTTPSConnection(GW_HOST, GW_PORT, timeout=10, context=ctx)
    return http.client.HTTPConnection(GW_HOST, GW_PORT, timeout=10)


def sender(worker_id: int):
    """Jeder Worker hat seine eigene Queue und eigene Keep-Alive Verbindung."""
    my_queue = worker_queues[worker_id]
    hdrs = {
        "Content-Type": "application/json",
        "X-Tenant-ID":  TENANT_ID,
        "User-Agent":   "bgp-stream/3.0",
        "Connection":   "keep-alive",
    }
    if API_KEY: hdrs["X-API-Key"] = API_KEY
    if JWT:     hdrs["Authorization"] = f"Bearer {JWT}"

    conn = _make_conn()
    batch = []
    deadline = time.monotonic() + BATCH_TIMEOUT_MS / 1000.0
    timeout = BATCH_TIMEOUT_MS / 1000.0

    while True:
        remaining = max(0.0005, deadline - time.monotonic())
        try:
            pl = my_queue.get(timeout=min(remaining, timeout))
            batch.append(pl)
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
                with _stats_lock:
                    stats["sent"] += len(batch)
            except Exception:
                try: conn.close()
                except: pass
                conn = _make_conn()
                with _stats_lock:
                    stats["errors"] += len(batch)
            batch = []
            deadline = time.monotonic() + BATCH_TIMEOUT_MS / 1000.0


def enqueue(pl):
    global _rr_counter
    if SAMPLE_RATE < 1.0 and random.random() > SAMPLE_RATE:
        return
    # Round-Robin über alle Worker-Queues
    with _rr_lock:
        idx = _rr_counter % WORKERS
        _rr_counter += 1
    try:
        worker_queues[idx].put_nowait(pl)
    except queue.Full:
        # Überlauf: nächsten Worker versuchen
        for i in range(1, WORKERS):
            try:
                worker_queues[(idx + i) % WORKERS].put_nowait(pl)
                return
            except queue.Full:
                continue
        with _stats_lock:
            stats["dropped"] += 1


def process(data):
    if not isinstance(data, dict): return
    ts       = data.get("timestamp", time.time())
    peer_asn = data.get("peer_asn", "0")
    path     = data.get("path", [])
    origin   = path[-1] if path else peer_asn

    for ann in data.get("announcements", []):
        nh = ann.get("next_hop", "")
        for pfx in ann.get("prefixes", []):
            origin_name = KNOWN_AS.get(str(origin), f"AS{origin}")
            peer_name   = KNOWN_AS.get(str(peer_asn), f"AS{peer_asn}")
            path_s = " → ".join(str(a) for a in (path or [])[-4:]) or str(peer_asn)
            try:
                ts_s = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(float(ts)))
            except Exception:
                ts_s = time.strftime("%Y-%m-%dT%H:%M:%SZ")
            enqueue({
                "level": "info",
                "message": f"ANNOUNCE {pfx} via {peer_name} (path: {path_s})",
                "source": "ripe-ris",
                "timestamp": ts_s,
                "metadata": {
                    "event": "ANNOUNCE", "prefix": pfx,
                    "peer_asn": str(peer_asn), "origin_asn": str(origin),
                    "origin": origin_name, "nexthop": nh,
                }
            })
            with _stats_lock: stats["ann"] += 1

    for pfx in data.get("withdrawals", []):
        peer_name = KNOWN_AS.get(str(peer_asn), f"AS{peer_asn}")
        try:
            ts_s = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(float(ts)))
        except Exception:
            ts_s = time.strftime("%Y-%m-%dT%H:%M:%SZ")
        enqueue({
            "level": "warn",
            "message": f"WITHDRAW {pfx} von {peer_name}",
            "source": "ripe-ris",
            "timestamp": ts_s,
            "metadata": {
                "event": "WITHDRAW", "prefix": pfx,
                "peer_asn": str(peer_asn), "origin_asn": str(origin),
                "origin": KNOWN_AS.get(str(origin), f"AS{origin}"), "nexthop": "",
            }
        })
        with _stats_lock: stats["with"] += 1


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
                "User-Agent: bgp-stream/3.0\r\n\r\n"
            ).encode())
            resp = b""
            while b"\r\n\r\n" not in resp:
                resp += ws.recv(4096)
            if b"101" not in resp:
                raise ConnectionError("Handshake failed")

            print("[stream] Verbunden ✓", flush=True)

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
                        print("[stream] Close frame — reconnect", flush=True)
                        pos = end; break
                    elif opcode == 9:
                        ws.sendall(bytes([0x8a, 0x00]))

                    pos = end

                if pos > 0:
                    del buf[:pos]

        except Exception as e:
            print(f"[stream] {e} — reconnect in 3s ...", flush=True)
            time.sleep(3)


def printer():
    prev_sent = 0
    while True:
        time.sleep(10)
        e = time.time() - stats["start"]
        with _stats_lock:
            s,er,dr,a,w = stats["sent"],stats["errors"],stats["dropped"],stats["ann"],stats["with"]
        delta = s - prev_sent
        prev_sent = s
        total_q = sum(q.qsize() for q in worker_queues)
        rps_window = delta / 10
        rps_total  = s / e if e > 0 else 0
        ingress = (a + w) / e if e > 0 else 0
        print(f"[stats] sent={s} | errors={er} | dropped={dr} | "
              f"rps_10s={rps_window:.0f}/s | rps_avg={rps_total:.0f}/s | "
              f"ingress={ingress:.0f}/s | queue={total_q}/{WORKERS*QUEUE_PER_WORKER} | "
              f"ann={a} | with={w} | elapsed={int(e)}s", flush=True)


if __name__ == "__main__":
    print("="*65)
    print("BGP Stream (RIPE NCC RIS Live) → Log Gateway  v3 (per-worker queues)")
    print("="*65)
    print(f"  Gateway:      {GATEWAY_URL}{BATCH_PATH}")
    print(f"  Workers:      {WORKERS}  (je eigene Queue × {QUEUE_PER_WORKER})")
    print(f"  Batch size:   {BATCH_SIZE}")
    print(f"  Batch tmo:    {BATCH_TIMEOUT_MS}ms")
    print(f"  Sample rate:  {SAMPLE_RATE*100:.0f}%")
    print(f"  API-Key:      {'ja' if API_KEY else 'NEIN'}")
    print(f"  JWT:          {'ja' if JWT else 'NEIN'}")
    print("="*65, flush=True)

    for i in range(WORKERS):
        threading.Thread(target=sender, args=(i,), daemon=True).start()
    threading.Thread(target=printer, daemon=True).start()

    bgp_stream()
