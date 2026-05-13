# page_fault — without pollard

## User

> I have a program `page_fault` and noticed it's slow. Please profile and find the root cause.

## Assistant

A cycles profile is a poor fit for a program that the kernel is doing the work for. I'll record on the `page-faults` software event instead:

```bash
$ perf record -e page-faults --call-graph dwarf -o /tmp/page_fault-slow.perf.data \
    ./target/demo/page_fault slow
mode=slow  iters=256  buf=16777216B  pages/iter=4096  elapsed=2.304s  acc=0x0000000007f80000
[ perf record: Captured and wrote 60.945 MB perf.data (7579 samples) ]
```

Three signals from that line alone:

* Wall clock is 2.3 s for 256 iterations of "touch every 4096th byte of a 16 MiB buffer". Pure userspace memory access at that pattern would be ≪10 ms — most of the time is in the kernel.
* perf collected **7579 page-fault samples** in 2.3 s. With `iters × pages/iter = 256 × 4096 ≈ 1.05 M` first-touch pages expected, the sampling rate is ~140×-down — so we're in the right ballpark.
* The fault count would be that high *only if every iteration re-faults the same pages*. A program that allocates once and reuses would fault ≈4096 pages total.

I can't query the perf.data directly. Two paths from here.

**Option A — convert and script.** `samply import` lands a Firefox-format JSON; a 30-line python script can walk its stack/frame/func/string tables and produce a top-N:

```bash
$ samply import /tmp/page_fault-slow.perf.data -o /tmp/page_fault-slow.json.gz --save-only
$ python3 transcripts/top_funcs.py /tmp/page_fault-slow.json.gz 5
   970  12.8%  0xb668
   967  12.8%  0xb640
   960  12.7%  0xb660
   953  12.6%  0xb630
   949  12.5%  0xb638
```

`samply --save-only` does no symbolication, so the names come out as raw addresses. Resolve manually:

```bash
$ addr2line -e ./target/demo/page_fault -f -C 0xb668 0xb640 0xb660 0xb630 0xb638
page_fault::touch
/home/moritz/dev/repos/pollard/example/src/bin/page_fault.rs:117
page_fault::touch
/home/moritz/dev/repos/pollard/example/src/bin/page_fault.rs:117
page_fault::touch
…
```

All five top addresses map to `page_fault::touch` line 117 — the store into the fresh page. Right answer, two ad-hoc scripts and an external lookup tool deep.

**Option B — open the UI.** `samply load /tmp/page_fault-slow.json.gz` opens the Firefox Profiler tab and you read the Call Tree off to me.

**Or just read the source.** `run_slow` calls `Scratch::new(BUF_BYTES)` *inside* the loop and drops it at the end of each iteration. `Scratch::new` issues `mmap(MAP_ANONYMOUS | MAP_PRIVATE)`, which hands back a region that has *no resident pages*; the kernel allocates and zeroes each page lazily on first write. `Drop` calls `munmap`, returning the pages to the kernel. So every iteration pays for ~4096 fresh page faults, ~1.05 M total over the run, lining up with the sample count.

`run_fast` does the same `Scratch::new` once before the loop. After the first iteration the pages are all resident; subsequent iterations write to warm pages and don't fault.

**Recommendation, provisional:** hoist the `Scratch::new` out of the loop in `run_slow`. Re-record with `perf record -e page-faults` and confirm the sample count drops by ~256×. I can't verify that here without you driving the UI or running another command.

## What this workflow misses

* Option A worked here because `touch` is short — five top addresses all resolved to the same line. For a longer hot function the addresses would smear across many instructions and the "they're all the same line" pattern would be hidden until I'd manually `addr2line`'d 20+ entries and grouped by file:line.
* The python script aggregates at the leaf frame only; there's no callsite or inline-chain awareness. If the hot leaf were `memset` called from three different sites, I couldn't tell which caller is driving the cost without a second pass that walks ancestor frames.
* The `page-faults` event is one of dozens (`minor-faults`, `major-faults`, `dTLB-load-misses`, `context-switches`, …). Switching events means re-recording and re-opening; with no programmatic query, the cost of "try another counter" is several minutes of scripting or UI navigation each time.
