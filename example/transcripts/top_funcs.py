#!/usr/bin/env python3
"""Ad-hoc: top-N functions by self samples from a samply / Firefox profile.

Used in the without-pollard transcripts as an alternative to driving the
Firefox Profiler UI. Output names are whatever the profile carries — when
samply records with --save-only it does no symbolication, so the names come
out as raw hex addresses that you have to resolve yourself with `addr2line`.

Usage: top_funcs.py <profile.json[.gz]> [N]
"""
import gzip
import json
import sys
from collections import Counter


def top_funcs(path, n=10):
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rt") as f:
        profile = json.load(f)
    rows = []
    for thread in profile["threads"]:
        names = thread["stringArray"]
        func_names = thread["funcTable"]["name"]
        frame_funcs = thread["frameTable"]["func"]
        stack_frame = thread["stackTable"]["frame"]
        samples = thread["samples"]["stack"]
        counts = Counter()
        for stack in samples:
            if stack is None:
                continue
            counts[names[func_names[frame_funcs[stack_frame[stack]]]]] += 1
        total = sum(counts.values())
        for fn, c in counts.most_common(n):
            rows.append((c, 100 * c / total if total else 0.0, fn))
    rows.sort(reverse=True)
    for c, pct, fn in rows[:n]:
        print(f"{c:>6}  {pct:5.1f}%  {fn}")


if __name__ == "__main__":
    top_funcs(sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 10)
