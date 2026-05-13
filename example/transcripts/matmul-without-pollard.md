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

I have a Firefox-format profile. I can't query it directly. Two paths.

**Option A — script the JSON.** A short python walk over `threads[].samples.stack[]` → `stackTable.frame` → `frameTable.func` → `funcTable.name` → `stringArray` gives a (function, count) histogram:

```bash
$ python3 transcripts/top_funcs.py /tmp/matmul-slow.json.gz 5
   372  13.1%  0xb7eb
   368  13.0%  0xb7d8
   366  12.9%  0xb7c0
   365  12.8%  0xb7ac
   356  12.5%  0xb7b7
```

`samply --save-only` skips symbolication, so I get raw addresses. Resolve:

```bash
$ addr2line -e ./target/demo/matmul -f -C 0xb7eb 0xb7d8 0xb7c0 0xb7ac 0xb7b7
matmul::matmul_slow
/home/moritz/dev/repos/pollard/example/src/bin/matmul.rs:22
matmul::matmul_slow
/home/moritz/dev/repos/pollard/example/src/bin/matmul.rs:22
matmul::matmul_slow
…
```

Top five addresses are five different instructions inside `matmul_slow`, all attributing to **line 22** — the inner-loop body `s += a[i*N+k] * b[k*N+j];`. That's the hot line.

**Option B — open the UI.** `samply load /tmp/matmul-slow.json.gz` and you read off the Call Tree.

**The code-only fallback if neither works:** the inner loop strides through `b` column-wise (`b[k * N + j]` where `j` is fixed in the outer scope and `k` is the loop variable). For `N=1280` and `f32`, each column step jumps 5120 bytes, which is one cache line per access — every load is a near-guaranteed L1 miss.

Option A landed on the right line; that the code-only reading agrees is a coincidence — and a useful sanity check rather than evidence on its own. If you had recorded with `perf record -e cache-misses` and converted via `samply import`, the profile would carry cache-miss counts as a separate marker series — but I have no way to query that hardware-counter axis from a JSON dump in a useful way (the python script aggregates one event series, with no view onto secondary markers).

Could you open the profile UI, switch the timeline source to **cache-misses**, and tell me which function dominates? If `matmul_slow` is at the top — much more so than it is for cycles — that confirms the loop-order theory.

Otherwise, my recommendation is provisional: try `ikj` loop order, re-measure.

## What this workflow misses

* The python+addr2line path landed the answer because `matmul_slow` is a leaf function with one hot statement and the cycles signal is strong enough that the top-5 addresses all hit it. For a function with multiple hot statements, or one that's split across an inlined helper, the same script would need a second pass to roll instructions up by `(file, line)` or by function.
* Hardware-counter axes (cache-misses, branch-misses, instructions) are still out of reach. The python script reads the default samples track; secondary marker series would need additional indexing code I'd have to write from scratch.
* I'm making a hypothesis from the source and asking you to confirm it from the UI. The profile is reduced to a yes/no oracle rather than a primary source of evidence.
