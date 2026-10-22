"""Check a generated co-op difficulty pack (built at 200%) with oead: files decompress, sizes unchanged, only Life differs.
Usage: python -I verify_difficulty_pack.py <pack folder>  (needs a venv with oead)."""
import oead, os, sys, glob
out = sys.argv[1]
B = os.path.expandvars(r"%LOCALAPPDATA%\com.privcord.app\native-games\botwmp")
srcs = [os.path.join(B, "app", "cemu_1.26.2", "graphicPacks", "BreathOfTheWild_BCML", "content"),
        os.path.join(B, "botw-game", "The Legend of Zelda_Breath of the Wild (EUR) Update v208", "content")]
def src(rel):
    for s in srcs:
        p = os.path.join(s, rel)
        if os.path.isfile(p):
            return p
bad = 0; n = 0
for p in glob.glob(os.path.join(out, "content", "Actor", "Pack", "*.sbactorpack")):
    rel = os.path.relpath(p, os.path.join(out, "content"))
    new = bytes(oead.yaz0.decompress(open(p, "rb").read()))
    old = bytes(oead.yaz0.decompress(open(src(rel), "rb").read()))
    diffs = [i for i in range(len(old)) if old[i] != new[i]] if len(old) == len(new) else None
    so, sn = oead.Sarc(old), oead.Sarc(new)
    for f in sn.get_files():
        if f.name.endswith(".bgparamlist"):
            a = oead.aamp.ParameterIO.from_binary(bytes(so.get_file(f.name).data)).objects["General"].params["Life"].v
            b = oead.aamp.ParameterIO.from_binary(bytes(f.data)).objects["General"].params["Life"].v
    if diffs is None or len(diffs) > 4 or b != (a * 2):
        bad += 1; print("BAD", rel, len(old), len(new), diffs and len(diffs), a, b)
    n += 1
ai_new = oead.byml.from_binary(oead.yaz0.decompress(open(os.path.join(out, "content", "Actor", "ActorInfo.product.sbyml"), "rb").read()))
ai_old = oead.byml.from_binary(oead.yaz0.decompress(open(src(r"Actor\ActorInfo.product.sbyml"), "rb").read()))
changed = 0
for x, y in zip(ai_old["Actors"], ai_new["Actors"]):
    assert str(x["name"]) == str(y["name"])
    for k in x.keys():
        if k == "generalLife" and int(x[k]) != int(y[k]):
            changed += 1
            if int(y[k]) != int(x[k]) * 2: print("BAD AI", x["name"])
raw_old = bytes(oead.yaz0.decompress(open(src(r"Actor\ActorInfo.product.sbyml"), "rb").read()))
raw_new = bytes(oead.yaz0.decompress(open(os.path.join(out, "content", "Actor", "ActorInfo.product.sbyml"), "rb").read()))
assert len(raw_old) == len(raw_new), "ActorInfo size changed"
diff_bytes = sum(1 for i in range(len(raw_old)) if raw_old[i] != raw_new[i])
assert diff_bytes <= 4 * changed, "ActorInfo changed outside generalLife"
print("actorinfo bytes changed", diff_bytes)
print("packs", n, "bad", bad, "actorinfo changed", changed, "count", len(ai_new["Actors"]), len(ai_old["Actors"]))
