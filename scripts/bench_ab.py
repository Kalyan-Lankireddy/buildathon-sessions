#!/usr/bin/env python3
"""Interleaved A/B of linux images: bench_ab.py VOLUME N IMG_A IMG_B [IMG_C ...]
Runs the images in turn (A, B, A, B, ...) so drifting machine load hits all of them equally.
Prints min and median of wall time and of user+sys CPU seconds."""
import os
import re
import statistics
import subprocess
import sys

vol, n, imgs = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
here = os.path.dirname(os.path.abspath(__file__))
res = {i: {"wall": [], "cpu": []} for i in imgs}
for _ in range(n):
    for img in imgs:
        out = subprocess.run([f"{here}/linux_run.sh", vol, "8"], capture_output=True, text=True,
                             env=dict(os.environ, IMG=img)).stderr
        w = re.search(r"total ([\d.]+)s", out)
        c = re.search(r"cpu user ([\d.]+)s sys ([\d.]+)s", out)
        if w and c:
            res[img]["wall"].append(float(w.group(1)))
            res[img]["cpu"].append(float(c.group(1)) + float(c.group(2)))
base = None
for img in imgs:
    wall, cpu = res[img]["wall"], res[img]["cpu"]
    if not wall:
        print(f"{img}: no samples")
        continue
    base = base or min(cpu)
    print(f"{img:<34} wall min {min(wall):.3f} med {statistics.median(wall):.3f} | cpu min {min(cpu):.2f} med {statistics.median(cpu):.2f} ({min(cpu)/base*100:.0f}% of first)")
