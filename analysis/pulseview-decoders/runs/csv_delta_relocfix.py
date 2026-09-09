#!/usr/bin/env python3
"""
Decode every .sr capture under
analysis/output/delta_singlewrite/task_a_b1b2_RELOCFIX/ into the same
column format as task_a_delta.csv.

This is the per-chunk RELOC_equiv dataset (see components/update/core/
src/delta.rs, Stage 3 marker placement, and Component.toml version-bump fix
in test/prepare_delta_eval.sh) -- the one to cite going forward.

Unlike csv_delta_markerfix.py, RELOC_equiv is summed across all of
its (now many) pulses per run, not just the last one: the marker fires
once per chunk (183 short pulses per run), so "the pulse" is the sum of
all of them, matching how the baseline's own per-chunk RELOC is summed.
Every other channel still fires exactly once per run, so last_pulse()
is equivalent to sum() for those.

Usage:
    python3 csv_delta_relocfix.py [output.csv] [data-root]
"""
import sys
import os
import csv
import glob
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_ROOT = os.path.normpath(os.path.join(
    HERE, "..", "..", "output", "delta_singlewrite", "task_a_b1b2_RELOCFIX"))

CHANNEL_NAMES = {
    5: "header_pull",
    6: "find_base",
    2: "masked_crc",
    3: "reconstruct_and_install",
    4: "INSTALL_equiv",
    7: "RELOC_equiv",
}


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
    edges = [[] for _ in range(8)]

    def flush(line, count):
        nonlocal t
        vals = line.split(",")
        for ch in range(8):
            v = int(vals[ch])
            if not edges[ch] or edges[ch][-1][1] != v:
                edges[ch].append((t, v))
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

    def pulses(ch_edges):
        out = []
        i = 0
        while i < len(ch_edges) - 1:
            ts, v = ch_edges[i]
            if v == 1:
                te, ve = ch_edges[i + 1]
                if ve == 0:
                    out.append((ts, te, te - ts))
            i += 1
        return out

    return [pulses(edges[ch]) for ch in range(8)]


def discover(root):
    files = []
    for sr_path in sorted(glob.glob(os.path.join(root, "**", "*.sr"), recursive=True)):
        op = os.path.relpath(os.path.dirname(sr_path), root)
        files.append((op, sr_path))
    return files


def main():
    out_csv = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "delta_relocfix_results.csv")
    root = sys.argv[2] if len(sys.argv) > 2 else DEFAULT_ROOT

    rows = []
    for op, path in discover(root):
        run = os.path.splitext(os.path.basename(path))[0]
        p = extract(path)

        row = {
            "dataset": "delta_singlewrite/task_a_b1b2_RELOCFIX",
            "operating_point": op,
            "run": run,
        }
        summed = {}
        for bit, name in CHANNEL_NAMES.items():
            total = sum(x[2] for x in p[bit])
            summed[name] = total
            row[f"{name}_s"] = f"{total:.6f}" if p[bit] else ""
            row[f"{name}_n_total"] = len(p[bit])

        hp, fb, mc, ri = (summed.get("header_pull"), summed.get("find_base"),
                          summed.get("masked_crc"), summed.get("reconstruct_and_install"))
        row["RECV_equiv_s"] = f"{hp+fb+mc+ri:.6f}" if all(x is not None for x in (hp, fb, mc, ri)) else ""

        rows.append(row)

    if not rows:
        print(f"No .sr files found under {root}")
        return

    fields = ["dataset", "operating_point", "run"]
    for name in CHANNEL_NAMES.values():
        fields += [f"{name}_s", f"{name}_n_total"]
    fields += ["RECV_equiv_s"]

    with open(out_csv, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for row in rows:
            w.writerow(row)
    print(f"Wrote {len(rows)} rows to {out_csv}")


if __name__ == "__main__":
    main()
