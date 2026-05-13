# page_fault — with pollard

## User

> I have a program `page_fault` and noticed it's slow. Please profile and find the root cause.

## Assistant

Wall-clock matters for "slow", but for a program whose name says "page_fault" the cycles axis will undercount: most of the time is in the kernel's fault handler, off-CPU from samply's perspective. I'll record on the `page-faults` software event with `perf` and import.

```bash
$ perf record -e page-faults --call-graph dwarf -o profiles/page_fault-slow.perf.data \
    ./target/demo/page_fault slow
mode=slow  iters=256  buf=16777216B  pages/iter=4096  elapsed=2.304s  acc=0x0000000007f80000
[ perf record: Captured and wrote 60.945 MB perf.data (7579 samples) ]

$ samply import profiles/page_fault-slow.perf.data \
    -o profiles/page_fault-slow.json.gz --save-only
event page-faults
```

7579 page-fault samples in 2.3 s. The program is supposed to touch `256 × 4096 = 1 048 576` pages first-time; at perf's default 4 kHz the recorded sample count is in the right order of magnitude.

### Tool call: `load_profile`

```
path: profiles/page_fault-slow.json.gz
name: page_fault-slow
```

```json
{
  "profile_id": "6a097815",
  "description": {
    "duration_ms": 2303,
    "interval_ms": 0.25,
    "total_samples": 7579,
    "unsymbolicated_pct": 0.05,
    "processes": [{"name": "page_fault",
                   "threads": [{"name": "page_fault", "samples": 7579}]}]
  }
}
```

### Tool call: `top_functions limit=10`

```
profile_id: 6a097815
```

| rank | self_pct | total_pct | function |
|-----:|---------:|----------:|---|
| 1 | **99.9** | 99.9 | `page_fault::main` |
| 2 |   0.0   |   0.0 | `0xffffffff8fd77440` (kernel) |
| 3 |   0.0   |   0.1 | `EntryPoint` (ld-linux) |
| 4 |   0.0   |   0.0 | `dl_start` (ld-linux) |

99.9 % of the page-fault events attribute to `page_fault::main`. `run_slow` and `touch` were inlined into `main` under the `demo` profile — function-level attribution rolls them up. Drop to per-line.

### Tool call: `source_for_function function="page_fault::main" with_samples=true whole_file=true`

```text
line | self_pct | code
  89 |    0.0  | // --- slide-sized core: slow (fresh mmap per iteration) ---
  90 |    0.0  | fn run_slow() -> u64 {
  91 |    0.0  |     let mut acc: u64 = 0;
  92 |    0.0  |     for iter in 0..ITERS {
  93 |    0.0  |         let mut scratch = Scratch::new(BUF_BYTES);
  94 |    0.0  |         acc = acc.wrapping_add(touch(scratch.as_mut_slice(), iter));
  95 |    0.0  |     }
  …
 112 |    0.0  | fn touch(buf: &mut [u8], iter: usize) -> u64 {
 113 |    0.0  |     let mark = (iter & 0xff) as u8;
 114 |    0.0  |     let mut sum: u64 = 0;
 115 |    0.0  |     let mut i = 0;
 116 |    0.0  |     while i < buf.len() {
 117 |  100.0  |         buf[i] = mark;
 118 |    0.0  |         sum = sum.wrapping_add(buf[i] as u64);
 119 |    0.0  |         i += PAGE;
```

### Diagnosis

100 % of page-fault events land on **line 117**: `buf[i] = mark;` — the first write to each page in the scratch buffer.

`Scratch::new` does `mmap(MAP_ANONYMOUS | MAP_PRIVATE)`, which returns a region with no resident pages. The first write to each page triggers a minor fault: the kernel allocates a fresh zero page, maps it, and returns. That's the per-page cost line 117 is charged with.

The bug is at line 93: `Scratch::new(BUF_BYTES)` *inside* the loop. Each iteration mmaps fresh pages, faults all 4096, then `Drop` calls `munmap` and the pages go back to the kernel. Next iteration repeats.

### Fix

Hoist the `Scratch::new` out of the loop:

```rust
fn run_fast() -> u64 {
    let mut scratch = Scratch::new(BUF_BYTES);
    let mut acc: u64 = 0;
    for iter in 0..ITERS {
        acc = acc.wrapping_add(touch(scratch.as_mut_slice(), iter));
    }
    acc
}
```

Same arithmetic, same output (`acc=0x07f80000` either way — `touch` only reads what it just wrote, so prior buffer contents don't matter). The fault budget collapses to ~4096 one-shot faults on the first iteration.

### After-fix profile

```bash
$ perf record -e page-faults --call-graph dwarf -o profiles/page_fault-fast.perf.data \
    ./target/demo/page_fault fast
mode=fast  iters=256  buf=16777216B  pages/iter=4096  elapsed=21.1ms  acc=0x0000000007f80000
[ perf record: Captured and wrote 0.329 MB perf.data (37 samples) ]
```

### Tool call: `compare_profiles profile_id_a="6a097815" profile_id_b="dbff808f" limit=5`

| rank | function | a_self | b_self | Δ_self_samples |
|-----:|---|---:|---:|---:|
| 1 | `page_fault::main` | 7571 | 29 | **−7542** |
| 2 | `_brk` (libc) | 0 | 1 | +1 |
| 3 | `sysmalloc` (libc) | 0 | 1 | +1 |

`page_fault::main` drops from 7571 page-fault samples to 29 — a 261× reduction, matching the 256× theoretical ratio of `iters × pages` (slow) to one-shot first-iter faults (fast). Wall clock follows: 2303 ms → 21 ms.

The two new entries in `_brk` / `sysmalloc` are libc's heap setup; they appear because the slow profile spent so much time in mmap that those startup faults were rounding error, while in the fast profile they're a measurable fraction of the (very small) total.

### Why `page-faults` over cycles

A cycles profile would still flag `page_fault::main` as the hot function — but it would charge time to `clear_huge_page` or `__memset_avx2` deep inside the kernel-zeroing path, with attribution diluted across whichever fault-handler path the kernel happened to be in when the sample fired. The `page-faults` event answers the question directly: "where are faults coming from?" — and the answer here is one line of user code.
