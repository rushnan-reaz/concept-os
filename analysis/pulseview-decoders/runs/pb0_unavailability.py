#!/usr/bin/env python3
"""Extract the PB0 (component-level unavailability) pulse width from a
D2-D6 combined-marker capture. D6 = PB0, high from Task::begin_update()
to Task::end_update().

Usage: pb0_unavailability.py <file.sr> [<file2.sr> ...]
"""
import sys
import subprocess


def extract(path):
    proc = subprocess.run(["sigrok-cli", "-i", path, "-O", "csv"],
                           capture_output=True, text=True, check=True)
    lines = [l for l in proc.stdout.splitlines()
             if l and not l.startswith(";") and not l.startswith("logic")]
    rate = None
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
            elif "Hz" in val:
                val = val.replace("Hz", "").strip()
            rate = float(val) * mult
    if rate is None:
        rate = 2_000_000
    dt = 1.0 / rate

    t = 0.0
    prev_line = None
    run_len = 0
    d6_edges = []

    def flush(line, count):
        nonlocal t
        vals = line.split(",")
        d6 = int(vals[4])  # D2,D3,D4,D5,D6 -> index 4
        if not d6_edges or d6_edges[-1][1] != d6:
            d6_edges.append((t, d6))
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

    # find first rising then falling edge pair
    pulses = []
    i = 0
    while i < len(d6_edges) - 1:
        ts, v = d6_edges[i]
        if v == 1:
            te, ve = d6_edges[i + 1]
            if ve == 0:
                pulses.append((ts, te, te - ts))
        i += 1
    return pulses


def main():
    for path in sys.argv[1:]:
        pulses = extract(path)
        if not pulses:
            print(f"{path}: NO PULSE FOUND")
            continue
        for ts, te, dur in pulses:
            print(f"{path}: unavailability = {dur*1e6:.1f} us  ({ts:.4f}s -> {te:.4f}s)")


if __name__ == "__main__":
    main()
