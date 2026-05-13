# Pipeline performance investigation

> I have a program `pipeline` and noticed it's slow. Please profile and find the root cause.

## Setup

Loaded the slow-mode profile.

```
load_profile path=/tmp/claude-1000/pf/pipeline-slow.json.gz
  -> profile_id=abcc0844
     duration=1883ms, 1827 samples @ 1kHz, process pipeline (tid 7)
     unsymbolicated=0.005% (good)
```

## Triage: top self-time

```
top_functions process=pipeline limit=20
```

Self-time leaders (slow profile, 1827 samples):

| rank | function                                     | self% | self_n |
|------|----------------------------------------------|-------|--------|
| 1    | `pipeline::rollup_slow`                      | 19.4  | 354    |
| 2    | `core::slice::sort::unstable::quicksort`     | 18.0  | 328    |
| 3    | `core::slice::sort::shared::small_sort_*`    |  9.4  | 171    |
| 4    | `<String as fmt::Write>::write_str`          |  8.8  | 160    |
| 5    | `alloc::fmt::format::format_inner`           |  8.7  | 159    |
| 6    | `<Formatter>::pad_integral`                  |  5.3  |  96    |
| 8    | `<u32 as fmt::Display>::fmt`                 |  4.8  |  87    |
| 10   | `pipeline::route_slow`                       |  2.6  |  48    |
| 11   | `__libc_malloc`                              |  2.2  |  40    |

Three distinct cost clusters: a leaf `rollup_slow` hotspot, a sort cluster, and a `fmt::format` + malloc cluster.

Rolling up by user function via `sort_by=total`:

| function                | self% | self_n | total% | total_n |
|-------------------------|-------|--------|--------|---------|
| `pipeline::route_slow`  |   2.6 |     48 |   45.9 |     839 |
| `pipeline::digest_slow` |   0.0 |      0 |   28.6 |     523 |
| `pipeline::rollup_slow` |  19.4 |    354 |   19.4 |     354 |

The three slow stages dominate the run: 45.9 + 28.6 + 19.4 = 93.9% of all samples.

## Defect 1 — `route_slow`: heap-alloc per event (45.9% total, 839 samples)

```
source_for_function function=pipeline::route_slow
```

Per-line attribution inside `route_slow`:

| line | code                                              | self% (of fn) | samples |
|------|---------------------------------------------------|---------------|---------|
|  81  | `let key = format!("{}:{}", e.host, e.metric);`   | 92.6          |   777   |
|  82  | `for b in key.bytes() {`                          |  3.2          |    27   |
|  85  | (loop branch)                                     |  4.1          |    34   |

Line 81 takes 777 / 839 = 92.6% of the function. The descendants are `format_inner`, `<String as fmt::Write>::write_str`, `Display::fmt`, plus `__libc_malloc` / `__libc_free` (visible in the global top-N). One `format!` per event x 12M events = 12M `String` allocations to immediately byte-sum and drop.

Fix: bypass `format!` and the `String` allocation entirely — sum the ASCII bytes of the decimals directly. `route_fast` already implements this:

```rust
fn digit_byte_sum(mut n: u32) -> u64 { /* sums '0'..='9' bytes of n */ }
fn route_fast(events: &[Event]) -> u64 {
    let mut acc: u64 = 0;
    for e in events {
        acc = acc.wrapping_add(digit_byte_sum(e.host));
        acc = acc.wrapping_add(b':' as u64);
        acc = acc.wrapping_add(digit_byte_sum(e.metric as u32));
    }
    acc
}
```

## Defect 2 — `digest_slow`: full sort per percentile (28.6% total, 523 samples)

```
source_for_function function=pipeline::digest_slow
```

| line | code                                  | self% (of fn) | samples |
|------|---------------------------------------|---------------|---------|
| 119  | `let mut sorted: Vec<u32> = chunk.to_vec();` | 0.8           |     4   |
| 120  | `sorted.sort_unstable();`             | 99.2          |   519   |

Line 120 takes 519 / 523 = 99.2% of the function and matches the rank-2/3 sort frames in the global top-N. Three calls per window x 600 windows = 1800 full sorts of 20k elements where each call reads exactly one index. `sort_unstable` is O(n log n) but a single percentile only needs O(n).

Fix: sort once into a reused buffer with `select_nth_unstable`, which partitions in O(n). `digest_fast` does this:

```rust
let mut buf: Vec<u32> = chunk.to_vec();
let n = buf.len();
for &pct in &[1usize, 50, 99] {
    let idx = n * pct / 100;
    let (_, v, _) = buf.select_nth_unstable(idx);
    acc = acc.wrapping_add(*v as u64);
}
```

(The fast version still re-runs `select_nth_unstable` three times per window — one true partial sort per percentile — but each is O(n), not O(n log n), and it avoids re-cloning the buffer.)

## Defect 3 — `rollup_slow`: column-major scan over row-major grid (19.4% self, 354 samples)

```
source_for_function function=pipeline::rollup_slow
```

| line | code                                                       | self% (of fn) | samples |
|------|------------------------------------------------------------|---------------|---------|
| 150  | `for r in 0..GRID_ROWS {`                                  |  0.8          |     3   |
| 151  | `acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);`  | 99.2          |   351   |

This function is a leaf — 354 self == 354 total — so the cost is the loads themselves, not callees. Loop order is `for c { for r { grid[r * GRID_COLS + c] } }` over a row-major `[GRID_ROWS=2048][GRID_COLS=4096]` `u32` grid. Stride between successive loads is `GRID_COLS * 4 = 16 KiB` — every load misses L1 (and likely L2). 354 samples = ~354 ms of cache-bound load latency.

Fix: swap the loops so the inner index varies the contiguous dimension. `rollup_fast`:

```rust
for r in 0..GRID_ROWS {
    for c in 0..GRID_COLS {
        acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
    }
}
```

Unit-stride access lets the hardware prefetcher saturate L1 bandwidth.

## Validation: fast profile

Recorded fast mode with samply and diffed against slow:

```
samply record -o pipeline-fast.json.gz -- ./target/demo/pipeline fast
  -> elapsed=273ms (vs slow ~1830ms self-reported, ~4.7x wall speedup)
load_profile -> profile_id=d72dfa31, 401ms total, 347 samples
compare_profiles a=abcc0844 b=d72dfa31 sort_by=delta_ms filter=re:pipeline::(route|digest|rollup)
```

| function                | a_total_ms | b_total_ms | delta_ms |
|-------------------------|-----------:|-----------:|---------:|
| `pipeline::route_slow`  |        839 |          0 |     -839 |
| `pipeline::digest_slow` |        523 |          0 |     -523 |
| `pipeline::rollup_slow` |        354 |          0 |     -354 |
| `pipeline::route_fast`  |          0 |        135 |     +135 |
| `pipeline::digest_fast` |          0 |         60 |      +60 |
| `pipeline::rollup_fast` |          0 |         15 |      +15 |

All three slow paths disappear; their fast counterparts take 135 + 60 + 15 = 210 ms versus the slow trio's 839 + 523 + 354 = 1716 ms — ~8.2x reduction on the stage budget, ~4.7x on wall time (the remainder is `generate_events` + `generate_grid`, which both modes share).
