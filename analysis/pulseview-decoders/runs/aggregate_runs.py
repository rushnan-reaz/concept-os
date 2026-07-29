#!/usr/bin/env python3
"""
Aggregate a set of .sr marker captures into mean +/- spread per metric.

Usage:
    python3 aggregate_runs.py <dir-or-files...>
    python3 aggregate_runs.py analysis/output/ConceptOS/VCP_115200
    python3 aggregate_runs.py run01.sr run03.sr

Assumes the full-component marker map: D2=RECV, D3=INSTALL, D4=MIGRATE, D5=RELOC.
A run is COMPLETE only if RECV, INSTALL, and MIGRATE each have exactly one pulse
(i.e. the capture outlasted the update). Clipped runs are reported and excluded
from the statistics.
"""
import sys, os, glob, zipfile, configparser
from statistics import mean, stdev

MAP = {"D2": "RECV", "D3": "INSTALL", "D4": "MIGRATE", "D5": "RELOC"}

def parse_samplerate(s):
    s = s.strip(); num = "".join(c for c in s if c.isdigit() or c == ".")
    unit = s[len(num):].strip().lower()
    return float(num) * {"hz":1,"khz":1e3,"mhz":1e6,"ghz":1e9}.get(unit, 1)

def load(path):
    z = zipfile.ZipFile(path); cp = configparser.ConfigParser()
    cp.read_string(z.read("metadata").decode())
    dev = [s for s in cp.sections() if s.startswith("device")][0]
    sr = parse_samplerate(cp[dev].get("samplerate", "1 Hz"))
    unit = int(cp[dev].get("unitsize", "1"))
    probes = {int(k[5:])-1: v for k, v in cp[dev].items() if k.startswith("probe")}
    chunks = sorted((n for n in z.namelist() if n.startswith("logic-1-")),
                    key=lambda n: int(n.split("-")[-1]))
    raw = b"".join(z.read(n) for n in chunks)
    return sr, unit, probes, raw

def pulses(raw, unit, bit, sr):
    off, mask, dt = bit // 8, 1 << (bit % 8), 1.0 / sr
    try:
        import numpy as np
        d = np.frombuffer(raw, dtype=np.uint8)
        if unit > 1: d = d[off::unit]
        b = ((d & mask) != 0).astype(np.int8)
        diff = np.diff(b)
        rises = list(np.where(diff == 1)[0] + 1)
        falls = list(np.where(diff == -1)[0] + 1)
    except ImportError:
        rises, falls, prev = [], [], None
        for i in range(off, len(raw), unit):
            v = 1 if raw[i] & mask else 0; si = (i-off)//unit
            if prev is not None and v != prev:
                (rises if v == 1 else falls).append(si)
            prev = v
    widths, fi = [], 0
    for r in rises:
        while fi < len(falls) and falls[fi] <= r: fi += 1
        if fi < len(falls): widths.append((falls[fi]-r)*dt)
    return widths

def metrics(path):
    sr, unit, probes, raw = load(path)
    name2bit = {}
    for bit, raw_name in probes.items():
        name2bit[MAP.get(raw_name, raw_name)] = bit
    out = {}
    for sig in ("RECV", "INSTALL", "MIGRATE", "RELOC"):
        if sig in name2bit:
            w = pulses(raw, unit, name2bit[sig], sr)
            out[sig] = w
    return out

def fmt(t):
    if t is None: return "-"
    if t == 0: return "0"
    if t < 1e-3: return f"{t*1e6:.1f}us"
    if t < 1.0:  return f"{t*1e3:.3f}ms"
    return f"{t:.4f}s"

def collect(args):
    files = []
    for a in args:
        if os.path.isdir(a): files += sorted(glob.glob(os.path.join(a, "*.sr")))
        else: files.append(a)
    return files

def main():
    if len(sys.argv) < 2:
        print(__doc__); sys.exit(1)
    files = collect(sys.argv[1:])
    if not files:
        print("no .sr files found"); sys.exit(1)

    rows, complete = [], []
    for f in files:
        m = metrics(f)
        recv = m.get("RECV", []); inst = m.get("INSTALL", [])
        mig = m.get("MIGRATE", []); rel = m.get("RELOC", [])
        ok = len(recv) == 1 and len(inst) == 1 and len(mig) == 1
        row = {
            "file": os.path.basename(f), "ok": ok,
            "RECV": recv[0] if recv else None,
            "INSTALL": inst[0] if inst else None,
            "MIGRATE": mig[0] if mig else None,
            "RELOC_total": sum(rel) if rel else None,
            "RELOC_n": len(rel),
        }
        rows.append(row)
        if ok: complete.append(row)

    print(f"{'file':<14}{'ok':<5}{'RECV':>12}{'INSTALL':>11}{'MIGRATE':>11}"
          f"{'RELOC_tot':>12}{'RELOC_n':>9}")
    print("-" * 74)
    for r in rows:
        print(f"{r['file']:<14}{('yes' if r['ok'] else 'CLIP'):<5}"
              f"{fmt(r['RECV']):>12}{fmt(r['INSTALL']):>11}{fmt(r['MIGRATE']):>11}"
              f"{fmt(r['RELOC_total']):>12}{r['RELOC_n']:>9}")

    if len(complete) < 1:
        print("\nNo complete runs to aggregate."); return
    print(f"\n=== mean +/- sd over {len(complete)} complete run(s) ===")
    def agg(key):
        vals = [r[key] for r in complete if r[key] is not None]
        if not vals: return
        m = mean(vals); s = stdev(vals) if len(vals) > 1 else 0.0
        cv = (100*s/m) if m else 0.0
        print(f"  {key:<12} {fmt(m):>12}  +/- {fmt(s):>10}   (CV {cv:4.1f}%, n={len(vals)})")
    for k in ("RECV", "INSTALL", "MIGRATE", "RELOC_total"):
        agg(k)
    ns = [r["RELOC_n"] for r in complete]
    print(f"  {'RELOC_n':<12} {mean(ns):>12.1f}  (min {min(ns)}, max {max(ns)})")

if __name__ == "__main__":
    main()
