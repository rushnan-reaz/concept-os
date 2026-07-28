#!/usr/bin/env python3
"""Decode a component-ID capture (D2-D5 = PC4-7 4-bit ID, D6 = MIGRATE/PB14)
and report the longest gap during which bthermo (component id 10) is not
scheduled -- i.e. component-level unavailability (Task B1,
MEASUREMENT_PROGRAM.md).

Usage: unavailability_gap.py <file.sr> [--rate 2000000] [--bthermo-id 10]
"""
import sys
import subprocess
import argparse


def decode(path, rate, bthermo_id):
    proc = subprocess.run(
        ["sigrok-cli", "-i", path, "-O", "csv"],
        capture_output=True, text=True, check=True,
    )
    lines = proc.stdout.splitlines()
    rows = [l for l in lines if l and not l.startswith(";") and not l.startswith("logic")]

    t = 0.0
    dt = 1.0 / rate
    prev_line = None
    run_len = 0
    id_changes = []
    migrate_edges = []
    prev_id = None
    prev_migrate = None

    def flush(line, count):
        nonlocal t, prev_id, prev_migrate
        d2, d3, d4, d5, d6 = (int(x) for x in line.split(","))
        idval = d2 * 1 + d3 * 2 + d4 * 4 + d5 * 8
        if idval != prev_id:
            id_changes.append((t, idval))
            prev_id = idval
        if d6 != prev_migrate:
            migrate_edges.append((t, d6))
            prev_migrate = d6
        t += count * dt

    for line in rows:
        if line == prev_line:
            run_len += 1
            continue
        if prev_line is not None:
            flush(prev_line, run_len)
        prev_line = line
        run_len = 1
    if prev_line is not None:
        flush(prev_line, run_len)

    total_duration = t

    # Longest gap where bthermo (id==bthermo_id) is not the running id.
    bthermo_times = [ts for ts, idv in id_changes if idv == bthermo_id]
    gaps = []
    for i in range(1, len(bthermo_times)):
        gaps.append((bthermo_times[i] - bthermo_times[i - 1], bthermo_times[i - 1], bthermo_times[i]))
    gaps.sort(reverse=True)

    return {
        "total_duration": total_duration,
        "num_id_changes": len(id_changes),
        "num_migrate_edges": len(migrate_edges),
        "largest_gap": gaps[0] if gaps else None,
        "migrate_edges": migrate_edges,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("file")
    ap.add_argument("--rate", type=float, default=2_000_000)
    ap.add_argument("--bthermo-id", type=int, default=10)
    args = ap.parse_args()

    r = decode(args.file, args.rate, args.bthermo_id)
    gap = r["largest_gap"]
    print(f"{args.file}: duration={r['total_duration']:.3f}s "
          f"id_changes={r['num_id_changes']} migrate_edges={r['num_migrate_edges']}")
    if gap:
        dur, start, end = gap
        print(f"  largest bthermo-unavailable gap: {dur*1000:.2f} ms  ({start:.3f}s -> {end:.3f}s)")
    else:
        print("  no gap found (bthermo id never seen or only seen once)")


if __name__ == "__main__":
    main()
