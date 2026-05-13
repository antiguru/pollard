# `page_fault` investigation

> I have a program `page_fault` and noticed it's slow. Please profile and find the root cause.

## Setup

Profile already recorded:

* `/tmp/claude-1000/pf/slow.json.gz` — `perf record -e page-faults --call-graph dwarf` then `samply import`.
* Binary: `/home/moritz/dev/repos/pollard/target/demo/page_fault`.
* Source: `/home/moritz/dev/repos/pollard/example/src/bin/page_fault.rs`.

Note: samples are page-fault *events*, not cycles.

## Load and describe

`load_profile` returns `profile_id=6a097815`.

| field | value |
| --- | --- |
| duration_ms | 2303 |
| total_samples (page-faults) | 7579 |
| process | `page_fault` (pid 7.1), single thread |
| unsymbolicated_pct | 0.05 |

## Top functions

`top_functions(profile_id=6a097815, limit=15)`:

| rank | function | self% | self samples |
| --- | --- | --- | --- |
| 1 | `page_fault::main` | 99.9 | 7571 |

One function dominates. The body of `main` is just a dispatch + timing; `run_slow` / `run_fast` / `touch` were inlined into it, so re-run with `expand_inlines=true`:

`top_functions(profile_id=6a097815, limit=15, expand_inlines=true)`:

| rank | function | self% | self samples |
| --- | --- | --- | --- |
| 1 | `page_fault::touch` | 99.9 | 7571 |

7571 of 7579 page-fault events occur inside `touch`.

## Where in `touch`

`source_for_function(function="page_fault::touch", expand_inlines=true, with_samples=true)`:

```
line 112: fn touch(buf: &mut [u8], iter: usize) -> u64 {   0
line 113:     let mark = (iter & 0xff) as u8;              0
line 114:     let mut sum: u64 = 0;                        0
line 115:     let mut i = 0;                               0
line 116:     while i < buf.len() {                        0
line 117:         buf[i] = mark;                        7571   (100% of touch samples)
line 118:         sum = sum.wrapping_add(buf[i] as u64);    0
line 119:         i += PAGE;                               0
line 120:     }
```

Every sample lands on the *first write* into a freshly-mapped page (line 117). That is the kernel fault-in path — `do_anonymous_page` zeroing a new page on demand.

## Where in the caller

`source_for_function(function="run_slow", expand_inlines=true, with_samples=true)`:

```
line 90: fn run_slow() -> u64 {                                                0
line 91:     let mut acc: u64 = 0;                                             0
line 92:     for iter in 0..ITERS {                                            0
line 93:         let mut scratch = Scratch::new(BUF_BYTES);                    0
line 94:         acc = acc.wrapping_add(touch(scratch.as_mut_slice(), iter));  7571 (100%)
line 95:     }
```

All faults come from the loop body in `run_slow`, where `Scratch::new(16 MiB)` allocates a brand-new anonymous mapping every iteration (line 93) and `touch` walks every page (line 94).
`Scratch::drop` munmaps the region at the end of the iteration, so the next iteration's `mmap` returns fresh pages that must be faulted in again.

## Diagnosis

`BUF_BYTES = 16 MiB`, `PAGE = 4096`, `ITERS = 256`.
Each iteration touches `16 MiB / 4 KiB = 4096` pages, and 256 iterations × 4096 pages = 1,048,576 minor faults — that matches the profile (7571 events at the default `page-faults` sampling reservoir).
`run_slow` re-mmaps the scratch buffer inside the loop; `run_fast` already does the right thing by mmapping once outside the loop.

The bug is in `run_slow`: hoist the allocation out of the loop. The shape is exactly `run_fast`.

## Fix

```rust
fn run_slow() -> u64 {
    let mut scratch = Scratch::new(BUF_BYTES);   // mmap once
    let mut acc: u64 = 0;
    for iter in 0..ITERS {
        acc = acc.wrapping_add(touch(scratch.as_mut_slice(), iter));
    }
    acc
}
```

This is equivalent to the existing `run_fast`. The first iteration still pays the faults; subsequent iterations hit warm pages.

## Validation: record fast-mode and diff

```
perf record -e page-faults --call-graph dwarf -o fast.perf.data ./target/demo/page_fault fast
   mode=fast  iters=256  elapsed=20.46ms
   [perf record: 39 samples]
samply import fast.perf.data -o fast.json.gz --save-only
```

`load_profile(/tmp/claude-1000/pf/fast.json.gz)` -> `dbff808f`, duration 8 ms, 39 page-fault samples.

`compare_profiles(a=6a097815 slow, b=dbff808f fast, filter="page_fault", expand_inlines=true)`:

| function | a_self_samples | b_self_samples | delta_self_samples |
| --- | --- | --- | --- |
| `page_fault::touch` | 7571 | 31 | -7540 |
| `page_fault::run_slow` (total) | 7571 | 0 | -7571 |
| `page_fault::run_fast` (total) | 0 | 31 | +31 |

Wall-clock from the program's own `Instant`: slow ~2.3 s, fast ~20 ms — a ~110× speedup.
Page-fault event count: 7579 -> 39 (~194×).
The remaining 31 samples in fast mode are the unavoidable first-touch faults on the single 16 MiB buffer plus a handful of startup faults; that's the floor.

## Root cause

`run_slow` allocates a fresh 16 MiB anonymous mapping every iteration (`Scratch::new` inside the `for` loop) and immediately walks it page-by-page, so every iteration pays the full first-touch cost of 4096 minor page faults.
Hoisting the `Scratch::new` call out of the loop (as `run_fast` already does) eliminates 255/256 of those faults.
