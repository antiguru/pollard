# pipeline_inlined — slow-path investigation

> I have a program `pipeline_inlined` and noticed it's slow. Please profile and find the root cause.

## Load profile

`load_profile(/tmp/claude-1000/pf/pipeline_inlined-slow.json.gz)` → id `b1ec593f`,
4366 ms / 4266 samples on thread `pipeline_inlined`, sample rate 1 kHz.

## Function-level view (default — too coarse)

`top_functions(process=pipeline_inlined)` collapses the stage work into `main`:

| rank | function | self% |
|---|---|---|
| 1 | `pipeline_inlined::main` | 68.9 |
| 2 | `core::slice::sort::unstable::quicksort::quicksort` | 7.7 |
| 3 | `alloc::fmt::format::format_inner` | 3.7 |
| 4 | `core::slice::sort::shared::smallsort::small_sort_network` | 3.2 |
| 5 | `core::fmt::write` | 2.3 |

Stage functions are `#[inline(always)]`, so they don't appear as symbols.
The fmt machinery (rank 3, 5, 8, 9) and quicksort (rank 2, 4) point at two
of the three stages indirectly, but `main`'s 68.9% self is opaque.

## Inline-chain view

`top_functions(expand_inlines=true)` resolves DWARF inline chains:

| rank | function (inline-resolved) | self% |
|---|---|---|
| 1 | `<u64>::wrapping_add` | 48.0 |
| 2 | `pipeline_inlined::rollup_slow` | 15.9 (total 64.8) |
| 3 | `core::ptr::copy` | 5.8 |
| 4 | `<core::fmt::Arguments>::estimated_capacity` | 2.5 |

`rollup_slow` now surfaces with 64.8% total — already the dominant stage.
The `wrapping_add` leaf at 48% is the inner-loop accumulator, almost all of
it inside `rollup_slow`.

## Per-line attribution

`source_for_function(function=pipeline_inlined::main, whole_file=true,
with_samples=true)` pinpoints each defect:

| line | code | self% | samples |
|---|---|---|---|
| 69 | `let key = format!("{}:{}", e.host, e.metric);` | 18.8 | 802 |
| 109 | `sorted.sort_unstable();` | 11.4 | 487 |
| 140 | `acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);` | 63.9 | 2722 |

## Defects

### 1. `rollup_slow` — column-major traversal (line 140, 63.9% self)

The loop iterates `c` outer, `r` inner, so each step jumps `GRID_COLS * 4 = 16 KiB`
in memory. Across `2048 * 4096 * 6 ≈ 50M` reads that defeats the L1 prefetcher
and pulls a cache line per element. The `wrapping_add` leaf at 48% is the inner
accumulator stalling on these loads.

**Fix:** swap loop order to row-major (already implemented as `rollup_fast`,
lines 152–155):

```rust
for r in 0..GRID_ROWS {
    for c in 0..GRID_COLS {
        acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
    }
}
```

### 2. `route_slow` — per-event `format!` allocation (line 69, 18.8% self)

`format!("{}:{}", ...)` allocates a fresh `String` per event (12M times) just
to byte-sum its digits. The whole fmt subtree
(`format_inner` 3.7%, `core::fmt::write` 2.3%, `pad_integral` 1.8%,
`<u32 as Display>::fmt` 1.9%, `String::write_str` 1.9%, `RawVecInner::finish_grow` 1.2%,
`malloc` 1.0%, plus line 69's 18.8%) sums to ~32% of the profile.

**Fix:** byte-sum the decimal digits directly without materializing the string
(`route_fast` + `digit_byte_sum`, lines 79–100).

### 3. `digest_slow` — full sort per percentile (line 109, 11.4% self)

For each 20 000-element chunk, the slow path clones the slice and runs a full
`sort_unstable()` three times (1st/50th/99th percentile) — `quicksort` (7.7%)
+ `small_sort_network` (3.2%) confirm this. The chunk is sorted in full just
to read one element.

**Fix:** `select_nth_unstable` reuses one buffer and does an O(n) partial sort
per percentile (`digest_fast`, lines 119–131).

## Validation

Recorded `samply record` on `pipeline_inlined fast`:

* slow elapsed: **4366 ms** (printed by the program)
* fast elapsed: **206.5 ms** (~21x)

`compare_profiles(a=slow, b=fast, expand_inlines=true, sort_by=delta_ms)`:

| function | a_self_ms | b_self_ms | delta_self_ms |
|---|---:|---:|---:|
| `<u64>::wrapping_add` | 2046 | 35 | **−2011** |
| `pipeline_inlined::rollup_slow` (self) | 678 | 0 | **−678** |
| `core::ptr::copy` | 248 | 29 | −219 |
| `<core::fmt::Arguments>::estimated_capacity` | 105 | 0 | −105 |
| `<core::fmt::Formatter>::pad_integral` | 75 | 0 | −75 |
| `core::fmt::write` | 56 | 0 (total: 626 → 0) | −56 |

All three hotspots are eliminated in the fast build:
* the column-major `rollup` cost (`wrapping_add` + `rollup_slow` self ≈ 2724 ms) collapses,
* the fmt subtree (`format_inner` / `pad_integral` / `core::fmt::write` / `String::write_str` /
  `Arguments::estimated_capacity`) goes to zero,
* `core::ptr::copy` drops from 248 ms to 29 ms as the `chunk.to_vec()`-then-full-sort
  loop is replaced by one `select_nth_unstable` per chunk.
