import re, struct, sys, collections
data = open(sys.argv[1], "rb").read()
hdr = re.compile(rb"\[(\d\d:\d\d:\d\d\.\d{3})\] (C->S|S->C|PROXY) (\d+)B: ")
recs = []; i = 0
while True:
    m = hdr.search(data, i)
    if not m: break
    n = int(m.group(3)); payload = data[m.end():m.end()+n]
    recs.append((m.group(1).decode(), m.group(2).decode(), payload)); i = m.end() + n
cnt = collections.Counter((d, len(p)) for _, d, p in recs)
print("records:", cnt.most_common(12))
def floats(p, lo, hi, step=4):
    out = []
    for o in range(0, len(p) - 3, step):
        f = struct.unpack_from("<f", p, o)[0]
        if lo <= f <= hi: out.append((o, round(f, 3)))
    return out
def show(label, p):
    print(f"--- {label}: len {len(p)}; strings: {[s.decode() for s in re.findall(rb'[A-Za-z_][A-Za-z0-9_]{3,}', p)][:12]}")
    print("    y-like floats (100..400):", floats(p, 100, 400)[:16])
    print("    x-like floats (-1100..-900):", floats(p, -1100, -900)[:8], " z-like (1700..1900):", floats(p, 1700, 1900)[:8])
    print("    small nonzero floats (0.001..5):", floats(p, 0.001, 5)[:16])
c2s = [r for r in recs if r[1] == "C->S"]; s2c = [r for r in recs if r[1] == "S->C"]
for ts, d, p in c2s[-2:]: show(f"C->S {ts}", p)
for ts, d, p in s2c[-3:]: show(f"S->C {ts}", p)
# hexdump of the first 0x120 bytes of the last C->S and S->C
def hexd(p, n=0x140):
    for o in range(0, n, 16): print(f"    {o:04x}: {p[o:o+16].hex(' ')}  {''.join(chr(b) if 32 <= b < 127 else '.' for b in p[o:o+16])}")
print("--- last C->S head"); hexd(c2s[-1][2])
if s2c: print("--- last S->C head"); hexd(s2c[-1][2])
