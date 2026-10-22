"""Analyze pos-sampler.ps1 CSVs: remote puppet smoothness on one PC vs the player's true motion on the other.
usage: python -I analyze-motion.py <remote.csv> <truth.csv>"""
import csv, math, statistics as st, sys


def load(p):
    with open(p) as f:
        return [(float(r["utc_ms"]), int(r["frame"]), float(r["x"]), float(r["y"]), float(r["z"])) for r in csv.DictReader(f)]


def per_frame(rows):
    """Last position seen in each game frame -> [(t_ms, frame, x, z)]."""
    out = {}
    for t, f, x, y, z in rows:
        out[f] = (t, f, x, z)
    return [out[k] for k in sorted(out)]


def moving_span(fr, eps=0.02):
    """First/last frame index where the position moves."""
    idx = [i for i in range(1, len(fr)) if math.hypot(fr[i][2] - fr[i - 1][2], fr[i][3] - fr[i - 1][3]) > eps]
    return (idx[0], idx[-1]) if idx else (0, 0)


def report(name, rows):
    fr = per_frame(rows)
    a, b = moving_span(fr)
    seg = fr[a - 1:b + 1]
    steps = [math.hypot(seg[i][2] - seg[i - 1][2], seg[i][3] - seg[i - 1][3]) for i in range(1, len(seg))]
    dts = [seg[i][0] - seg[i - 1][0] for i in range(1, len(seg))]
    frame_ms = st.median(dts) if dts else 0
    still = sum(1 for s in steps if s < 0.005)
    moving = [s for s in steps if s >= 0.005]
    # Position updates inside the movement window (any row where the position changed).
    t0, t1 = seg[0][0], seg[-1][0]
    upd = [r for i, r in enumerate(rows) if i and t0 <= r[0] <= t1 and (r[2], r[4]) != (rows[i - 1][2], rows[i - 1][4])]
    gaps = [upd[i][0] - upd[i - 1][0] for i in range(1, len(upd))]
    print(f"== {name}: {len(fr)} frames, frame time median {frame_ms:.1f} ms (~{1000 / frame_ms if frame_ms else 0:.0f} fps)")
    print(f"   moving window {len(seg)} frames: frames with NO movement {still} ({100 * still / max(1, len(steps)):.0f} %)")
    if moving:
        print(f"   step per frame (m): median {st.median(moving):.3f}  p10 {sorted(moving)[len(moving) // 10]:.3f}  "
              f"p90 {sorted(moving)[len(moving) * 9 // 10]:.3f}  max {max(moving):.3f}  cv {st.pstdev(moving) / st.mean(moving):.2f}")
    if gaps:
        gs = sorted(gaps)
        print(f"   position changes: {len(upd)} in {t1 - t0:.0f} ms -> {1000 * len(upd) / max(1, t1 - t0):.0f}/s; "
              f"gap median {st.median(gaps):.1f} ms p90 {gs[len(gs) * 9 // 10]:.1f} max {gs[-1]:.1f}")
    return fr


def lag(remote, truth):
    """Delay (ms) that best aligns the remote's x(t)/z(t) with the truth, by brute force over 0..600 ms."""
    def interp(fr, t):
        lo, hi = 0, len(fr) - 1
        if t <= fr[0][0]:
            return fr[0][2], fr[0][3]
        if t >= fr[-1][0]:
            return fr[-1][2], fr[-1][3]
        while hi - lo > 1:
            m = (lo + hi) // 2
            if fr[m][0] <= t:
                lo = m
            else:
                hi = m
        a, b = fr[lo], fr[hi]
        k = (t - a[0]) / max(1e-9, b[0] - a[0])
        return a[2] + k * (b[2] - a[2]), a[3] + k * (b[3] - a[3])
    best = None
    ts = [r[0] for r in remote]
    for d in range(-200, 801, 5):
        err = 0.0
        for t in ts[::3]:
            rx, rz = interp(remote, t)
            tx, tz = interp(truth, t - d)
            err += (rx - tx) ** 2 + (rz - tz) ** 2
        if best is None or err < best[1]:
            best = (d, err)
    # Residual after the best shift = how far the puppet strays from the true path (m RMS).
    n = len(ts[::3])
    return best[0], math.sqrt(best[1] / n)


remote = report("remote puppet (main PC)", load(sys.argv[1]))
truth = report("true motion (STRIX)", load(sys.argv[2]))
d, rms = lag(remote, truth)
print(f"== best alignment: puppet trails the real player by ~{d} ms (clock-sync error included); RMS path error {rms:.2f} m")
