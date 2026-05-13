# matmul — without pollard

## User

> I have a program `matmul` and noticed it's slow. Please profile and find the root cause.

## Assistant

I'll record a sampling profile with samply:

```bash
$ samply record --save-only -o /tmp/matmul-slow.json.gz ./target/demo/matmul slow
mode=slow  N=1280  elapsed=2.047s  GFLOPS=2.05  trace=409706.0938
```

2 s of compute, ~2 GFLOPS. For a 1280³ multiply that's well below what a single core should do — peak for a modern Apple ARM core is in the 30–50 GFLOPS range for FMA-friendly f32 kernels, so we're leaving roughly 15× on the table.

I have a Firefox-format profile. I can't query it directly — I'd need to either:

* Open it in the Firefox Profiler UI (`samply load /tmp/matmul-slow.json.gz`) and ask you to read me the call tree, or
* Write fragile `jq` against the raw thread sample arrays to flatten stacks into a (function, count) list.

**Without you driving the UI, my best guess from the code is:** the inner loop strides through `b` column-wise (`b[k * N + j]` where `j` is fixed in the outer scope and `k` is the loop variable). For `N=1280` and `f32`, each column step jumps 5120 bytes, which is one cache line per access — every load is a near-guaranteed L1 miss.

But "best guess from reading the code" is not a profile-driven answer. It's the same answer I'd give without recording anything. The profile is collecting cache miss counts (samply records `cache-misses` events on Apple silicon), but I have no way to query that hardware-counter axis from a JSON dump in a useful way.

Could you open the profile UI, switch the timeline source to **cache-misses**, and tell me which function dominates? If `matmul_slow` is at the top — much more so than it is for cycles — that confirms the loop-order theory.

Otherwise, my recommendation is provisional: try `ikj` loop order, re-measure.

## What this workflow misses

* Hardware-counter axes (cache-misses, branch-misses, instructions) are the *whole point* of profiling memory-bound code, and the workflow has no programmatic access to them.
* I'm making a hypothesis from the source and asking you to confirm it from the UI. The profile is reduced to a yes/no oracle rather than a primary source of evidence.
* I can't get per-line attribution to confirm the bug is on the `s += a[…] * b[…]` line and not, say, on the `c[…] = s;` write or in the prelude.
