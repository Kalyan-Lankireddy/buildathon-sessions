#!/usr/bin/env python3
"""Run scripts/linux_run.sh N times and print per-stage minimum and median seconds.
Usage: bench_linux.py VOLUME [N] [CPUS]. The machine is shared and noisy, so judge by the min."""
import re
import statistics
import subprocess
import sys

vol = sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 5
cpus = sys.argv[3] if len(sys.argv) > 3 else "8"
here = __file__.rsplit("/", 1)[0]
rows = {}
order = []
for _ in range(n):
    out = subprocess.run([f"{here}/linux_run.sh", vol, cpus], capture_output=True, text=True).stderr
    for line in out.splitlines():
        m = re.match(r"\[\s*[\d.]+s\] (.+?)\s+([\d.]+)s\s", line)
        if m:
            k = m.group(1)
            if k not in rows:
                rows[k] = []
                order.append(k)
            rows[k].append(float(m.group(2)))
        m = re.match(r"total ([\d.]+)s", line)
        if m:
            rows.setdefault("TOTAL", []).append(float(m.group(1)))
        m = re.match(r"cpu user ([\d.]+)s sys ([\d.]+)s", line)
        if m:
            rows.setdefault("cpu user", []).append(float(m.group(1)))
            rows.setdefault("cpu sys", []).append(float(m.group(2)))
for k in order + ["TOTAL", "cpu user", "cpu sys"]:
    v = rows.get(k)
    if v:
        print(f"{k:<24} min {min(v):6.3f}  median {statistics.median(v):6.3f}")
