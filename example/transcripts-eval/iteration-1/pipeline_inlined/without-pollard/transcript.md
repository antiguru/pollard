> I have a program `pipeline_inlined` and noticed it's slow. Please profile and find the root cause.

## Setup

Profile: `/tmp/claude-1000/pf/pipeline_inlined-slow.json.gz` (samply, cycles, ~4266 samples on the worker thread).
Binary: `/home/moritz/dev/repos/pollard/target/demo/pipeline_inlined` (PIE, debug info present).
Stages are `#[inline(always)]`, so function-name attribution collapses into `main`.
Strategy: walk every sampled stack frame, expand inline chains with `addr2line -i`, attribute samples to source lines in `pipeline_inlined.rs`.

## Step 1 — Profile shape

```
$ python3 ... # decompress, count threads
thread 0: name=samply main=True samples=6
thread 1: name=pipeline_inlined main=True samples=4266
```

Leaf-frame histogram is uninformative — the top 20 leafs are all raw addresses inside `main` (samply didn't symbolicate inline frames), each holding ~2-3% of samples.

## Step 2 — Inline-expanded leaf attribution

Dump unique addresses across all stacks (404 of them) and run `addr2line -i -f -C` against the binary:

```
$ addr2line -i -f -C -e .../pipeline_inlined -a < pi_all_addrs.txt > pi_all_a2l.txt
```

For the hottest leaf address `0xd6c3` (122 samples, 2.9%):
```
<u64>::wrapping_add  core/src/num/uint_macros.rs:2457
pipeline_inlined::rollup_slow  pipeline_inlined.rs:140
pipeline_inlined::run_slow     pipeline_inlined.rs:166
pipeline_inlined::main         pipeline_inlined.rs:185
```

## Step 3 — Inclusive attribution by source line

For each sample, collect every `pipeline_inlined.rs` line that appears anywhere in the inline-expanded stack, count once per sample:

```
total samples: 4266

   2722   63.8%  line 140  rollup_slow      acc += grid[r*GRID_COLS + c]
    802   18.8%  line  69  route_slow       let key = format!("{}:{}", ...)
    487   11.4%  line 109  digest_slow      sorted.sort_unstable()
     60    1.4%  line  49  generate_events  .collect()
     43    1.0%  line 139  rollup_slow      inner loop r in 0..GRID_ROWS
     34    0.8%  line  73  route_slow       function body
     32    0.8%  line  70  route_slow       key.bytes()
```

Bucketed by stage (deepest user-source frame per sample):

```
 2765   64.8%  rollup_slow
  869   20.4%  route_slow
  488   11.4%  digest_slow
   60    1.4%  generate_events
   39    0.9%  generate_grid
   45    1.1%  other
```

Three distinct hot stages, each backed by a different bottleneck signature in leaf samples:
* `route_slow`: leafs in `<core::fmt::Arguments>::estimated_capacity` (2.5%), `pad_integral` (1.8%), `core::fmt::write` (1.3%), `alloc::alloc::alloc` (0.7%), `format_inner` (0.5%) — formatter + heap.
* `digest_slow`: leafs in `core::ptr::copy` (5.8%), `quicksort::partition_lomuto_branchless_cyclic` (0.6% + closure 0.6%), `smallsort::insert_tail` (0.8%), `<u32 as PartialOrd>::lt` (1.5%) — sort.
* `rollup_slow`: leafs are bare `wrapping_add` against grid loads, owning 63.8%.

## Defect 1 — `rollup_slow` column-major scan (line 140, ~64% of samples)

```rust
135: fn rollup_slow(grid: &[u32]) -> u64 {
136:     let mut acc: u64 = 0;
137:     for _ in 0..ROLLUP_PASSES {
138:         for c in 0..GRID_COLS {
139:             for r in 0..GRID_ROWS {
140:                 acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
```

Disassembly at hottest leaf `0xd6c3` confirms the stride:
```
d6c0:  mov    (%rcx,%rsi,1),%eax     ; load grid element
d6c3:  add    %r8,%rax               ; wrapping_add — hot
d6c6:  add    $0x10000,%rsi          ; stride = 0x10000 = 4096*4 bytes
d6cd:  cmp    $0x2000000,%rsi
d6d4:  jne    d6a0                   ; inner row loop
```

Outer index is the column, inner index walks rows with a 16-KiB stride. Each grid row is `4096 * 4 = 16384` bytes, larger than an L1 line and on the edge of L2 working-set for `2048 * 16384 = 32 MiB`. Every iteration misses cache.

**Fix:** swap loop order to row-major (already implemented as `rollup_fast` at lines 149-158): outer `r`, inner `c`, unit-stride loads.

## Defect 2 — `route_slow` allocates a String per event (line 69, ~19% of samples)

```rust
66: fn route_slow(events: &[Event]) -> u64 {
67:     let mut acc: u64 = 0;
68:     for e in events {
69:         let key = format!("{}:{}", e.host, e.metric);
70:         for b in key.bytes() {
71:             acc = acc.wrapping_add(b as u64);
```

`N_EVENTS = 12_000_000` allocations through `format!`. Leaf evidence: 2.5% in `Arguments::estimated_capacity`, 1.8% in `pad_integral`, 1.3% in `core::fmt::write`, 1.1% in `String as fmt::Write::write_str`, 0.7% in `alloc::alloc::alloc`, 0.5% in `format_inner`. None of the bytes are kept; only the byte-sum matters.

**Fix:** sum the decimal digits and the `:` separator directly without allocation (already implemented as `route_fast` + `digit_byte_sum` at lines 79-100).

## Defect 3 — `digest_slow` full-sorts per percentile (line 109, ~11% of samples)

```rust
104: fn digest_slow(values: &[u32]) -> u64 {
105:     let mut acc: u64 = 0;
106:     for chunk in values.chunks(WINDOW) {
107:         for &pct in &[1usize, 50, 99] {
108:             let mut sorted: Vec<u32> = chunk.to_vec();
109:             sorted.sort_unstable();
110:             let idx = sorted.len() * pct / 100;
```

For each `WINDOW = 20_000`-element chunk the code allocates and fully sorts three times to get three percentiles. Leaf evidence: 5.8% in `core::ptr::copy` (the partition swap), 1.5% in `<u32>::lt`, 0.6%+0.6% in `partition_lomuto_branchless_cyclic`, 0.8% in `smallsort::insert_tail`.

**Fix:** sort once and index three times, or use `select_nth_unstable` (O(n)) on a single reused buffer (already implemented as `digest_fast` at lines 119-131).

## Summary

| # | Line | Stage         | % samples | Defect                    | Fix                         |
|---|------|---------------|-----------|---------------------------|-----------------------------|
| 1 | 140  | `rollup_slow` | 63.8%     | column-major (16 KiB stride) | swap loop order to row-major |
| 2 | 69   | `route_slow`  | 18.8%     | `format!` per event       | inline digit byte-sum        |
| 3 | 109  | `digest_slow` | 11.4%     | full sort per percentile  | `select_nth_unstable` once   |

The three "fast" variants already in the file are the correct fixes; the slow ones are kept side-by-side to demonstrate each defect.

## Caveats

* Counts are sample counts on the cycles event; the rollup hotspot is memory-bound so a cache-miss event would skew the ratio further toward it.
* "Inclusive by source line" double-counts when a sample's stack visits two distinct user lines in the same call (rare here — `run_slow` calls each stage sequentially).
* `samply` did not symbolicate inline frames in the JSON itself, so all attribution went through `addr2line -i` on the binary; results match the function semantics on inspection.
