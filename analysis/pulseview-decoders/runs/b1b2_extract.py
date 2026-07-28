#!/usr/bin/env python3
"""Extract Task B1 (PB0, D6) and Task B2 (PB7, D7) markers from a combined
6-channel capture (D2-D5 classic phase markers, D6=PB0, D7=PB7).

Usage: b1b2_extract.py <file.sr>
"""
import sys
import subprocess


def extract(path):
    proc = subprocess.run(["sigrok-cli", "-i", path, "-O", "csv"],
                           capture_output=True, text=True, check=True)
    rate = 2_000_000
    for l in proc.stdout.splitlines():
        if l.startswith("; Samplerate:"):
            val = l.split(":")[1].strip()
            mult = 1
            if "MHz" in val:
                mult = 1_000_000
                val = val.replace("MHz", "").strip()
            elif "kHz" in val:
                mult = 1_000
                val = val.replace("kHz", "").strip()
            rate = float(val) * mult
    dt = 1.0 / rate

    lines = [l for l in proc.stdout.splitlines()
             if l and not l.startswith(";") and not l.startswith("logic")]

    t = 0.0
    prev_line = None
    run_len = 0
    d6_edges = []
    d7_edges = []

    def flush(line, count):
        nonlocal t
        vals = line.split(",")
        d6 = int(vals[4])
        d7 = int(vals[5])
        if not d6_edges or d6_edges[-1][1] != d6:
            d6_edges.append((t, d6))
        if not d7_edges or d7_edges[-1][1] != d7:
            d7_edges.append((t, d7))
        t += count * dt

    for line in lines:
        if line == prev_line:
            run_len += 1
            continue
        if prev_line is not None:
            flush(prev_line, run_len)
        prev_line = line
        run_len = 1
    if prev_line is not None:
        flush(prev_line, run_len)

    def pulses(edges):
        out = []
        i = 0
        while i < len(edges) - 1:
            ts, v = edges[i]
            if v == 1:
                te, ve = edges[i + 1]
                if ve == 0:
                    out.append((ts, te, te - ts))
            i += 1
        return out

    b1 = pulses(d6_edges)
    b2 = pulses(d7_edges)
    return b1, b2


def main():
    for path in sys.argv[1:]:
        b1, b2 = extract(path)
        b1_us = b1[0][2] * 1e6 if b1 else None
        total_b2 = sum(p[2] for p in b2)
        print(f"{path}: B1={b1_us:.1f}us" if b1_us else f"{path}: B1=NONE",
              f" B2_count={len(b2)} B2_total={total_b2*1e3:.2f}ms"
              + (f" B2_each=[{', '.join(f'{p[2]*1e6:.1f}' for p in b2)}]us" if b2 else ""))


if __name__ == "__main__":
    main()
