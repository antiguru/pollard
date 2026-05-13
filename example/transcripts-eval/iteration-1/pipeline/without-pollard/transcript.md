# pipeline investigation (without pollard MCP)

> I have a program `pipeline` and noticed it's slow. Please profile and find the root cause.

Profile and binary already on disk; only standard tools (`python3`, `addr2line`,
`readelf`, `nm`).
Profile is samply Firefox JSON (`/tmp/claude-1000/pf/pipeline-slow.json.gz`),
binary is `/home/moritz/dev/repos/pollard/target/demo/pipeline` (PIE, debug
info, demo profile).

## Step 1 — profile structure

```text
keys: ['meta','libs','threads','pages','profilerOverhead','counters']
threads: 2  (samply control thread + 'pipeline')
thread 1 'pipeline': 1827 samples, frameTable.length=395, funcTable.length=...
sample frames carry only raw `address` ints; funcTable.name = '0xXXXX'
profile is **not pre-symbolicated** (`meta.symbolicated=false`)
```

## Step 2 — raw leaf attribution (no symbols yet)

Walk `stackTable`/`samples`, count leaf frames by `(resource, address)`.
Top leaf addresses in the `pipeline` binary (sample counts):

```text
112  0xe688
 95  0xe671
 55  0x4318c
 53  0x4350f
 49  0x43516
 38  0xd552
 36  0xd4dc
 33  0xd5d2
 ...
```

PIE binary, segment vaddr 0, so frame addresses are file offsets — feed
directly to `addr2line`.

## Step 3 — symbolicate top addresses

```
$ awk '{print $2}' pipeline_leaf.txt | head -50 \
   | addr2line -f -i -C -e .../target/demo/pipeline -a
```

Excerpts:

```text
0x000000000000e688  core::slice::sort::unstable::quicksort::partition_lomuto_branchless_cyclic ...
                    -> quicksort::quicksort                       <-- digest_slow leaf
0x000000000004318c  <core::fmt::Arguments>::estimated_capacity
                    alloc::fmt::format::format_inner              <-- route_slow / format!()
0x000000000004350f  <alloc::string::String as core::fmt::Write>::write_str
0x000000000000d552  <u64>::wrapping_add
                    pipeline::rollup_slow  pipeline.rs:151        <-- rollup_slow leaf
0x000000000000c5f3  pipeline::route_slow   pipeline.rs:82         <-- route_slow leaf
```

libc leaves (`munmap`, `tcache_get`/`libc_malloc`, `libc_free`,
`__memcpy_avx_unaligned*`) confirm heavy allocator traffic.

## Step 4 — roll up to user functions

For every distinct pipeline-binary address, batch-`addr2line` and pick the
innermost `pipeline::*` frame; bucket samples.

```text
LEAF by user function (1827 samples):
   354 (19.4%)  pipeline::rollup_slow
   242 (13.2%)  core::ptr::copy
   113 ( 6.2%)  <String as fmt::Write>::write_str
   111 ( 6.1%)  fmt::Arguments::estimated_capacity
    96 ( 5.3%)  Formatter::pad_integral
    76 ( 4.2%)  pipeline::route_slow
    66 ( 3.6%)  <u32 as PartialOrd>::lt          (inside quicksort)
    45 ( 2.5%)  alloc::alloc::alloc
    ...

INCLUSIVE by user function:
  1745 (95.5%)  pipeline::run_slow
   839 (45.9%)  pipeline::route_slow             <-- Stage A
   523 (28.6%)  pipeline::digest_slow            <-- Stage B
   454 (24.8%)  core::slice::sort::unstable::quicksort::quicksort
   354 (19.4%)  pipeline::rollup_slow            <-- Stage C
```

Three independent peaks, summing to ~94% of the inclusive runtime; each
sits in a different stage of `run_slow`.

## Step 5 — line-level leaf attribution

Walk every stack, find the innermost `pipeline.rs` frame, bucket by `file:line`:

```text
777  pipeline::route_slow     pipeline.rs:81    let key = format!("{}:{}", e.host, e.metric);
519  pipeline::digest_slow    pipeline.rs:120   sorted.sort_unstable();
351  pipeline::rollup_slow    pipeline.rs:151   acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
 34  pipeline::route_slow     pipeline.rs:85    (end of loop)
 28  pipeline::generate_events  ...:61          (one-time setup, not a hot stage)
```

Line attribution lines up exactly with the three stages.

---

## Defects

### Defect 1 — Stage A `route_slow`, pipeline.rs:81 (allocator)

```rust
for e in events {
    let key = format!("{}:{}", e.host, e.metric);   // 12M heap allocations
    for b in key.bytes() { acc = acc.wrapping_add(b as u64); }
}
```

Evidence:
* 839 inclusive samples (45.9%) under `route_slow`.
* Of those, the leaf time fans out into `estimated_capacity`, `write_str`,
  `pad_integral`, `u32::_fmt_inner`, `alloc::alloc::alloc`, and libc
  `__libc_malloc`/`__libc_free`/`munmap`/`__memcpy_avx_unaligned*` — the
  classic `format!()` + drop-`String` per iteration signature.
* `core::ptr::copy` at 13.2% leaf is dominated by `String`/`Vec` growth
  inside the format machinery.

Fix: don't materialise a `String`. Compute the byte-sum directly from the
integers (matches `route_fast`/`digit_byte_sum` in source):

```rust
acc = acc.wrapping_add(digit_byte_sum(e.host));
acc = acc.wrapping_add(b':' as u64);
acc = acc.wrapping_add(digit_byte_sum(e.metric as u32));
```

### Defect 2 — Stage B `digest_slow`, pipeline.rs:120 (algorithm)

```rust
for chunk in values.chunks(WINDOW) {
    for &pct in &[1usize, 50, 99] {
        let mut sorted: Vec<u32> = chunk.to_vec();
        sorted.sort_unstable();                     // O(n log n) per pct
        let idx = sorted.len() * pct / 100;
        acc = acc.wrapping_add(sorted[idx] as u64);
    }
}
```

Evidence:
* 523 inclusive samples (28.6%) in `digest_slow`; 454 of those (24.8%)
  inside `quicksort::quicksort` / `partition_lomuto_branchless_cyclic`
  (the addresses `0xe688`/`0xe671`/`0xe669`/`0xe68f`).
* 519 line-level samples on the `sort_unstable()` call site.
* Full sort done three times per window for a single index read.

Fix: partial selection, and reuse the buffer (matches `digest_fast`):

```rust
let mut buf: Vec<u32> = chunk.to_vec();
let n = buf.len();
for &pct in &[1usize, 50, 99] {
    let idx = n * pct / 100;
    let (_, v, _) = buf.select_nth_unstable(idx);     // O(n)
    acc = acc.wrapping_add(*v as u64);
}
```

`select_nth_unstable` is O(n); also one `to_vec` per window instead of
three (further cuts allocator and memcpy traffic visible in `core::ptr::copy`).

### Defect 3 — Stage C `rollup_slow`, pipeline.rs:151 (memory layout)

```rust
for _ in 0..ROLLUP_PASSES {
    for c in 0..GRID_COLS {           // outer: column
        for r in 0..GRID_ROWS {       // inner: row
            acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
        }
    }
}
```

Evidence:
* 354 leaf samples (19.4%) sit on the load+add at line 151, even though
  the loop body is just an addition — a CPU-bound add would burn ~zero
  cycles per iteration; the samples mean the load is stalled.
* The grid is `GRID_ROWS=2048 x GRID_COLS=4096` `u32` = 32 MiB, far
  larger than L2/L3. Stride between consecutive inner-loop reads is
  `GRID_COLS * 4 = 16 KiB`, blowing every cache level on every step.
* No allocator or library frames under `rollup_slow` — pure load stall.

We don't have a cache-miss event in this profile (only cycles), so the
"misses" claim is inferred from the access pattern + cycle attribution,
not from a PMU counter. A `perf stat -e LLC-load-misses,...` run would
confirm.

Fix: swap loop order so the inner loop walks unit stride
(matches `rollup_fast`):

```rust
for r in 0..GRID_ROWS {
    for c in 0..GRID_COLS {
        acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
    }
}
```

## Gaps / honesty notes

* Profile is cycles only; the cache claim for Defect 3 is inferred from
  the access pattern and sample concentration on the load, not measured.
* `pipeline::main` shows 16 leaf samples at line 205; that's the
  `println!` formatting `Duration`, not interesting.
* `generate_events` / `generate_grid` together account for ~50 leaf
  samples (~3%) — one-time setup, ignored.
