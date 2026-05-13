# page_fault investigation (without pollard)

> I have a program `page_fault` and noticed it's slow. Please profile and find the root cause.

## Inputs

* Profile (Firefox JSON): `/tmp/claude-1000/pf/slow.json.gz`
* Raw perf data: `/tmp/claude-1000/pf/slow.perf.data`
* Binary: `/home/moritz/dev/repos/pollard/target/demo/page_fault`
* Event recorded: `page-faults` (software event), not cycles.
  Samples count fault events, not time.

## Step 1: top of profile with `perf report`

```
$ perf report -i slow.perf.data --stdio --no-children --percent-limit 1
# Samples: 7K of event 'page-faults'
# Event count (approx.): 1053550
    99.98%  page_fault  page_fault  [.] page_fault::main
            |
            ---page_fault::touch (inlined)
               page_fault::run_slow (inlined)
               page_fault::main
               ...
```

One stack absorbs essentially all events: `touch` inlined into `run_slow` inlined into `main`.
Event count: ~1.05M page faults across ~2.3 s of sampling.

## Step 2: contrast with fast profile

```
$ perf report -i fast.perf.data --stdio --no-children --percent-limit 1
# Samples: 37  of event 'page-faults'
# Event count (approx.): 4200
    95.26%  page_fault  page_fault  [.] page_fault::main
            ---page_fault::touch (inlined)
               page_fault::run_fast (inlined)
               page_fault::main
```

Fast mode: ~4.2K fault events.
Slow mode: ~1.05M.
Ratio ≈ 250×.
Both run the same `touch` loop; only the caller (`run_slow` vs `run_fast`) differs.

## Step 3: where in `touch` do faults land?

`perf script` over the slow profile, grouping leaf addresses:

```
$ perf script -i slow.perf.data | awk '/^[^\t]/{getline; print}' | sort | uniq -c | sort -rn | head
    970   5572859c3668 page_fault::touch+0x298 (inlined)
    967   5572859c3640 page_fault::touch+0x270 (inlined)
    960   5572859c3660 page_fault::touch+0x290 (inlined)
    953   5572859c3630 page_fault::touch+0x260 (inlined)
    949   5572859c3638 page_fault::touch+0x268 (inlined)
    947   5572859c3650 page_fault::touch+0x280 (inlined)
    922   5572859c3658 page_fault::touch+0x288 (inlined)
    903   5572859c3648 page_fault::touch+0x278 (inlined)
```

All hot addresses are inside `touch`, tightly clustered.
Resolve them to lines:

```
$ nm target/demo/page_fault | grep ' T main'
000000000000b3d0 T main          # offset 0x270 / 0x298 etc. = page_fault::main+...
$ addr2line -e target/demo/page_fault -f -i -C 0xb668 0xb640 0xb630
page_fault::touch       page_fault.rs:117
page_fault::run_slow    page_fault.rs:94
page_fault::main        page_fault.rs:128
```

Line 117 is the per-page write `buf[i] = mark`.
Line 94 is `Scratch::new(BUF_BYTES)` inside the `run_slow` loop.

## Step 4: numbers sanity-check

```
BUF_BYTES   = 16 MiB
PAGE        = 4 KiB
pages/iter  = 16 MiB / 4 KiB = 4096
ITERS       = 256
expected total faults ≈ 4096 × 256 = 1,048,576
observed (perf event count)        = 1,053,550
```

Matches within ~0.5 %.
That is the entire population of samples.

## Diagnosis

`run_slow` calls `Scratch::new(BUF_BYTES)` *inside* the per-iteration loop:

```rust
fn run_slow() -> u64 {
    let mut acc: u64 = 0;
    for iter in 0..ITERS {
        let mut scratch = Scratch::new(BUF_BYTES);       // fresh 16 MiB mmap
        acc = acc.wrapping_add(touch(scratch.as_mut_slice(), iter));
    }
    acc
}
```

`Scratch::new` is an anonymous `mmap(MAP_PRIVATE | MAP_ANONYMOUS)`.
The mapping has no physical pages backing it until first touch.
`touch` then writes one byte to every 4 KiB page, faulting in 4096 fresh zero-filled pages each iteration.
`Scratch::drop` calls `munmap`, throwing the work away before the next iteration redoes it.

Result: ~1 M minor page faults instead of ~4 k.
Each fault drags the process into the kernel to allocate, zero, and install a page — work that dominates the wall-clock cost even though anonymous faults are cheap individually.

`run_fast` hoists the `mmap` out of the loop: pages are faulted in once on iteration 0, every later iteration writes to already-resident pages.

## Fix

Move the allocation out of the hot loop (i.e. use the `run_fast` shape).
Minimal patch:

```rust
fn run_slow() -> u64 {
    let mut scratch = Scratch::new(BUF_BYTES);   // hoist out of loop
    let mut acc: u64 = 0;
    for iter in 0..ITERS {
        acc = acc.wrapping_add(touch(scratch.as_mut_slice(), iter));
    }
    acc
}
```

If callers genuinely need the buffer reset between iterations, either:

1. `MADV_DONTNEED` the region after each iteration (cheaper than full mmap/munmap, still releases pages), or
2. just overwrite the contents — the existing `touch` already writes every page that it later reads, so a zeroing pass isn't required for correctness.

Both keep the mapping alive and skip the per-iteration fault storm.

## Gaps / honest notes

* Without pollard's MCP I queried the profile via `perf report` / `perf script` and resolved addresses with `addr2line`.
  That worked here because one stack dominates; for a flatter profile I'd want a programmatic top-functions / call-tree query instead of eyeballing `perf report` output.
* I didn't inspect the JSON-format profile (`slow.json.gz`) directly — `perf.data` had everything needed.
  Parsing the Firefox JSON manually with `jq`/python would be the fallback if only the JSON were available.
