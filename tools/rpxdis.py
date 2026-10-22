"""Decompress an RPX (Wii U ELF) in memory and disassemble address ranges (PowerPC32 BE).
usage: python -I rpxdis.py <rpx> <addr>[:+len|-end] ...   (hex addresses)
"""
import struct, sys, zlib
from capstone import Cs, CS_ARCH_PPC, CS_MODE_32, CS_MODE_BIG_ENDIAN

def load(path):
    d = open(path, "rb").read()
    assert d[:4] == b"\x7fELF"
    e_shoff, = struct.unpack(">I", d[0x20:0x24])
    e_shentsize, e_shnum = struct.unpack(">HH", d[0x2e:0x32])
    secs = []
    for i in range(e_shnum):
        o = e_shoff + i * e_shentsize
        name, typ, flags, addr, off, size = struct.unpack(">IIIIII", d[o:o + 24])
        if typ == 0 or addr == 0 or size == 0:
            continue
        raw = d[off:off + size]
        if flags & 0x08000000:
            usz, = struct.unpack(">I", raw[:4])
            raw = zlib.decompress(raw[4:])
            assert len(raw) == usz, (i, len(raw), usz)
        secs.append((addr, raw, flags))
    return secs

def read(secs, addr, n):
    for a, raw, _ in secs:
        if a <= addr < a + len(raw):
            return raw[addr - a: addr - a + n]
    raise KeyError(hex(addr))

def main():
    secs = load(sys.argv[1])
    md = Cs(CS_ARCH_PPC, CS_MODE_32 | CS_MODE_BIG_ENDIAN); md.skipdata = True
    for spec in sys.argv[2:]:
        if ":+" in spec:
            a, n = spec.split(":+"); a = int(a, 16); n = int(n, 16)
        elif "-" in spec:
            a, e = spec.split("-"); a = int(a, 16); n = int(e, 16) - a
        else:
            a = int(spec, 16); n = 0x40
        code = read(secs, a, n)
        print(f"; ---- {a:08x}..{a+n:08x}")
        for ins in md.disasm(code, a):
            print(f"{ins.address:08x}  {ins.bytes.hex():8s}  {ins.mnemonic:8s} {ins.op_str}")

if __name__ == "__main__":
    main()
