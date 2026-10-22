"""List RPX import stubs (symbol name -> stub address) matching a substring.
usage: python -E rpx-imports.py <rpx> <substring>..."""
import struct, sys, zlib

d = open(sys.argv[1], "rb").read()
e_shoff, = struct.unpack(">I", d[0x20:0x24])
e_shentsize, e_shnum, e_shstrndx = struct.unpack(">HHH", d[0x2e:0x34])
secs = []
for i in range(e_shnum):
    o = e_shoff + i * e_shentsize
    name, typ, flags, addr, off, size, link, info, align, entsize = struct.unpack(">IIIIIIIIII", d[o:o + 40])
    raw = d[off:off + size]
    if flags & 0x08000000 and size >= 4:
        raw = zlib.decompress(raw[4:])
    secs.append(dict(typ=typ, addr=addr, raw=raw, link=link, entsize=entsize))
for s in secs:
    if s["typ"] != 2:  # SHT_SYMTAB
        continue
    strtab = secs[s["link"]]["raw"]
    raw = s["raw"]
    for i in range(0, len(raw), 16):
        st_name, st_value, st_size, st_info, st_other, st_shndx = struct.unpack(">IIIBBH", raw[i:i + 16])
        name = strtab[st_name:strtab.index(b"\0", st_name)].decode(errors="replace")
        if any(k in name for k in sys.argv[2:]):
            print(f"{st_value:08x} size={st_size} info={st_info:#x} shndx={st_shndx} {name}")

# Call sites: relocations (SHT_RELA) whose symbol matches.
for s in secs:
    if s["typ"] != 4:  # SHT_RELA
        continue
    symtab = secs[s["link"]]
    strtab = secs[symtab["link"]]["raw"]
    raw = s["raw"]
    for i in range(0, len(raw), 12):
        r_offset, r_info, r_addend = struct.unpack(">IIi", raw[i:i + 12])
        si = r_info >> 8
        st_name = struct.unpack(">I", symtab["raw"][si * 16:si * 16 + 4])[0]
        name = strtab[st_name:strtab.index(b"\0", st_name)].decode(errors="replace")
        if any(k == name for k in sys.argv[2:]):
            print(f"  call/ref at {r_offset:08x} type={r_info & 0xff} -> {name}")
