#!/usr/bin/env python3
"""
Compare multiple operating points (BT@9600, VCP@115200, VCP@9600, ...) side by side.

Each operating point is a directory of .sr captures, as produced by capture_runs.sh
(analysis/output/ConceptOS/<LABEL>/run01.sr ...). The directory's basename is used
as the label unless overridden with label:path.

Usage:
    python3 compare_operating_points.py <dir-or-label:dir> [<dir-or-label:dir> ...]
    python3 compare_operating_points.py analysis/output/ConceptOS/BT_9600 \
                                         analysis/output/ConceptOS/VCP_115200 \
                                         analysis/output/ConceptOS/VCP_9600

Reuses aggregate_runs.py's capture parsing so both scripts stay consistent.
"""
import sys
import os
from statistics import mean, stdev

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from aggregate_runs import collect, metrics, fmt  # noqa: E402

METRIC_KEYS = ("RECV", "INSTALL", "MIGRATE", "RELOC_total")


def load_operating_point(path):
    files = collect([path])
    if not files:
        return None
    rows = []
    for f in files:
        m = metrics(f)
        recv = m.get("RECV", [])
        inst = m.get("INSTALL", [])
        mig = m.get("MIGRATE", [])
        rel = m.get("RELOC", [])
        ok = len(recv) == 1 and len(inst) == 1 and len(mig) == 1
        rows.append({
            "ok": ok,
            "RECV": recv[0] if recv else None,
            "INSTALL": inst[0] if inst else None,
            "MIGRATE": mig[0] if mig else None,
            "RELOC_total": sum(rel) if rel else None,
        })
    complete = [r for r in rows if r["ok"]]
    return {"n_total": len(rows), "n_complete": len(complete), "rows": complete}


def agg(rows, key):
    vals = [r[key] for r in rows if r[key] is not None]
    if not vals:
        return None
    m = mean(vals)
    s = stdev(vals) if len(vals) > 1 else 0.0
    cv = (100 * s / m) if m else 0.0
    return m, s, cv, len(vals)


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)

    points = []
    for arg in sys.argv[1:]:
        if ":" in arg and not os.path.exists(arg):
            label, path = arg.split(":", 1)
        else:
            path = arg
            label = os.path.basename(os.path.normpath(path))
        data = load_operating_point(path)
        if data is None:
            print(f"warning: no .sr files found for '{label}' at {path}", file=sys.stderr)
            continue
        points.append((label, data))

    if not points:
        print("No operating points with data found.")
        sys.exit(1)

    # Header
    col_w = 22
    header = f"{'metric':<12}" + "".join(f"{label:>{col_w}}" for label, _ in points)
    print(header)
    print("-" * len(header))

    for key in METRIC_KEYS:
        line = f"{key:<12}"
        for _, data in points:
            a = agg(data["rows"], key)
            if a is None:
                line += f"{'-':>{col_w}}"
            else:
                m, s, cv, n = a
                cell = f"{fmt(m)} (cv{cv:.1f}%)" if n > 1 else fmt(m)
                line += f"{cell:>{col_w}}"
        print(line)

    print()
    n_line = f"{'n (used)':<12}"
    for _, d in points:
        cell = "{}/{}".format(d["n_complete"], d["n_total"])
        n_line += f"{cell:>{col_w}}"
    print(n_line)


if __name__ == "__main__":
    main()
