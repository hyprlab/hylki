#!/usr/bin/env python3
"""
tools/span_report.py — Analyzes stage latency telemetry spans and calculates p50, p95, p99 percentiles.
Usage:
    python3 tools/span_report.py [logfile]
    journalctl -u hylki | python3 tools/span_report.py
"""

import sys
import re
import math
from collections import defaultdict

SPAN_REGEX = re.compile(r'stage="?([a-zA-Z0-9_:-]+)"?.*elapsed_us=([0-9]+)')

def percentile(values, p):
    if not values:
        return 0.0
    k = (len(values) - 1) * p
    f = math.floor(k)
    c = math.ceil(k)
    if f == c:
        return float(values[int(k)])
    d0 = values[int(f)] * (c - k)
    d1 = values[int(c)] * (k - f)
    return float(d0 + d1)

def main():
    spans = defaultdict(list)

    if len(sys.argv) > 1 and sys.argv[1] != "-":
        with open(sys.argv[1], "r", encoding="utf-8", errors="ignore") as f:
            lines = f.readlines()
    else:
        lines = sys.stdin.readlines()

    for line in lines:
        match = SPAN_REGEX.search(line)
        if match:
            stage = match.group(1)
            elapsed_us = int(match.group(2))
            spans[stage].append(elapsed_us)

    if not spans:
        print("No stage span telemetry entries found.")
        return

    print("=" * 86)
    print(f"{'STAGE':<30} {'COUNT':>8} {'P50 (ms)':>11} {'P95 (ms)':>11} {'P99 (ms)':>11} {'MAX (ms)':>11}")
    print("-" * 86)

    for stage, samples in sorted(spans.items()):
        samples.sort()
        count = len(samples)
        p50 = percentile(samples, 0.50) / 1000.0
        p95 = percentile(samples, 0.95) / 1000.0
        p99 = percentile(samples, 0.99) / 1000.0
        max_val = samples[-1] / 1000.0

        print(f"{stage:<30} {count:>8} {p50:>11.2f} {p95:>11.2f} {p99:>11.2f} {max_val:>11.2f}")

    print("=" * 86)

if __name__ == "__main__":
    main()
