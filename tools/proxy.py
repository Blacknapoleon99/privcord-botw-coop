"""Logging TCP proxy: listen on 127.0.0.1:5051, forward to 127.0.0.1:5050, dump both directions to a raw log.
Record format: b"[HH:MM:SS.mmm] C->S 1234B: " + raw payload + b"\n"."""
import socket, threading, sys, datetime
LISTEN = ("127.0.0.1", 5051); TARGET = ("127.0.0.1", 5050)
log = open(sys.argv[1] if len(sys.argv) > 1 else "proxy.log", "ab", buffering=0)
lock = threading.Lock()

def dump(tag, data):
    ts = datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3]
    with lock:
        log.write(f"[{ts}] {tag} {len(data)}B: ".encode("ascii") + data + b"\n")

def pump(src, dst, tag):
    try:
        while True:
            d = src.recv(65536)
            if not d:
                break
            dump(tag, d); dst.sendall(d)
    except OSError:
        pass
    finally:
        for s in (src, dst):
            try: s.shutdown(socket.SHUT_RDWR)
            except OSError: pass

def handle(c):
    s = socket.create_connection(TARGET)
    threading.Thread(target=pump, args=(c, s, "C->S"), daemon=True).start()
    threading.Thread(target=pump, args=(s, c, "S->C"), daemon=True).start()

srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(LISTEN); srv.listen(5)
dump("PROXY", b"listening")
while True:
    c, _ = srv.accept(); dump("PROXY", b"client connected"); handle(c)
