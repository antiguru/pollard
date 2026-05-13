# pipeline_inlined: investigating slow mode without pollard

> I have a program `pipeline_inlined` and noticed it's slow. Please profile and find the root cause.

The profile `/tmp/claude-1000/pf/pipeline_inlined-slow.json.gz` is a samply / Firefox-format JSON.
No pollard MCP available, so I parse the JSON directly and symbolicate with `addr2line`.

## 1. Profile shape

```
$ jq -c 'keys' pipeline_inlined-slow.json
["counters","libs","meta","pages","profilerOverhead","threads"]
$ jq '.threads | length' ...   # 2 (samply self + pipeline_inlined)
$ jq -c '.threads[1] | {samples: .samples.length, frames: .frameTable.length}' ...
{"samples":4266,"frames":404}
```

Function names in the profile are bare hex offsets (`"0xd9fb"`, `"0xe2a8"` ...) — samply did not symbolicate inline.
Frames carry `address` (lib-relative file offset) and `resource` (which lib).
Resource 1 = `pipeline_inlined`. So I bulk-feed unique addresses to `addr2line -fi` against the binary.

## 2. Spot-check: which line is the dominant leaf?

```
$ addr2line -e .../pipeline_inlined -fip 0xd478 0xd56a 0xd5e3
... rollup_slow at pipeline_inlined.rs:140
    (inlined by) run_slow at pipeline_inlined.rs:166
    (inlined by) main at pipeline_inlined.rs:185
```

So the `0xd4..–0xd6..` cluster (50+ frames) all sits at `pipeline_inlined.rs:140` inside `rollup_slow`, inlined through `run_slow` into `main`. Inlining hides it from a function-table view — addr2line restores it.

## 3. Per-stage inclusive attribution

I walk every sample's call stack, find the innermost frame whose addr2line inline-chain mentions a user stage function (`route_slow`, `digest_slow`, `rollup_slow`, ...).

```
$ python3 .claude-tmp-stage.py /tmp/claude-1000/pipeline_inlined-slow.json
Total weight: 4266, unattributed: 27 (0.6%)

== Stage inclusive attribution ==
   2765   64.8%  rollup_slow
    869   20.4%  route_slow
    488   11.4%  digest_slow
     60    1.4%  generate_events
     39    0.9%  generate_grid
     18    0.4%  run_slow
```

97% of cycles are in three user stages. Per-stage line breakdown:

```
-- rollup_slow (2765) --   2722 @ line 140       43 @ line 139
-- route_slow  (869)  --    802 @ line  69       34 @ line 73    32 @ line 70
-- digest_slow (488)  --    487 @ line 109        1 @ line 108
```

Three distinct hotspots, one per stage. Per-stage leaf attribution confirms what the cycles are spent on:

```
rollup_slow : 2044 wrapping_add, 678 rollup_slow body, 40 PartialOrd::lt
route_slow  : 112 libc, 109 pad_integral, 107 Arguments, 63 __fmt_inner,
              59 MaybeUninit, 56 core::fmt::write, 46 String::write_str,
              33 RawVec, 30 alloc::alloc
digest_slow : 248 core::ptr::copy, 66 PartialOrd::lt, 35 quicksort,
              33 smallsort::insert_tail, 18 ptr::read, ...
```

## 4. Defect 1 — `rollup_slow` column-major traversal (line 140, 63.8%)

Source:

```rust
// line 134-145
fn rollup_slow(grid: &[u32]) -> u64 {
    let mut acc: u64 = 0;
    for _ in 0..ROLLUP_PASSES {
        for c in 0..GRID_COLS {            // outer column
            for r in 0..GRID_ROWS {        // inner row  <-- stride GRID_COLS*4 = 16 KiB
                acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
            }
        }
    }
    acc
}
```

Inner stride is `GRID_COLS * 4 = 16 KiB`, so every load misses L1 / L2.
Grid size = 2048×4096×4 B = 32 MiB, far beyond L2 → constant DRAM traffic.

Disassembly at the hot region confirms strided loads (`0x4000` = one row):
```
d470: mov -0xc000(%rcx,%rsi,1),%r9d
d47b: mov -0x8000(%rcx,%rsi,1),%eax
d482: mov -0x4000(%rcx,%rsi,1),%r8d
d490: mov     0(%rcx,%rsi,1),%eax
d496: add $0x10000,%rsi                  ; advance 4 rows
```

**Fix:** swap the loops — `for r in 0..GRID_ROWS { for c in 0..GRID_COLS { ... } }` (this is exactly `rollup_fast` in the file). Unit-stride access; autovectorisable. Expected ~10× speedup of this stage.

## 5. Defect 2 — `route_slow` allocates a `format!` string per event (line 69, 18.8%)

Source:

```rust
// line 66-75
fn route_slow(events: &[Event]) -> u64 {
    let mut acc: u64 = 0;
    for e in events {
        let key = format!("{}:{}", e.host, e.metric);   // <-- 12M allocs + fmt
        for b in key.bytes() {
            acc = acc.wrapping_add(b as u64);
        }
    }
    acc
}
```

12 M events × `format!` → 12 M `String` allocations plus the full `core::fmt` machinery.
Leaf breakdown (libc + pad_integral + Arguments + alloc + write_str ≈ 80% of the stage) is the classic `format!`-in-a-loop signature.

**Fix:** compute the byte-sum of the decimal digits without materialising a string. `route_fast` already does this with a `digit_byte_sum` helper. Removes all allocations and the formatter; should drop this stage by ~50×.

## 6. Defect 3 — `digest_slow` full sort per percentile (line 109, 11.4%)

Source:

```rust
// line 103-115
fn digest_slow(values: &[u32]) -> u64 {
    let mut acc: u64 = 0;
    for chunk in values.chunks(WINDOW) {
        for &pct in &[1usize, 50, 99] {
            let mut sorted: Vec<u32> = chunk.to_vec();
            sorted.sort_unstable();                     // <-- full sort, 3× per window
            let idx = sorted.len() * pct / 100;
            acc = acc.wrapping_add(sorted[idx] as u64);
        }
    }
    acc
}
```

Each 20 000-element window is fully sorted three times (once per percentile) and the buffer is reallocated each time. Leaf samples sit in quicksort/insert_tail/`ptr::copy` — clearly sort work, not the surrounding loop.

**Fix:** allocate the buffer once per window and use `select_nth_unstable` for each percentile (O(n) per call instead of O(n log n) and no repeated alloc). `digest_fast` already shows this pattern. Expected ≈ 5–10× speedup of this stage.

## 7. Verification & gaps

* `wrapping_add` in `rollup_slow` accounts for 48% of all samples (single line in the disassembled inner loop) — perfectly consistent with stalled loads on a 16 KiB stride.
* Without pollard, this required hand-rolled Python over the Firefox JSON plus `addr2line -i` to undo inlining; pollard's `top_groups by source_line` / `source_for_function` / `expand_inlines` would have produced the per-line breakdown directly.
* Sampling rate is the samply default (1 kHz). 4 266 samples ⇒ ~0.1 s resolution; sub-1% attributions are noisy but the three hotspots are far above that floor.

## 8. Summary of fixes

| # | Stage         | Line | %     | Defect                                  | Fix                                          |
|---|---------------|------|-------|-----------------------------------------|----------------------------------------------|
| 1 | `rollup_slow` | 140  | 63.8% | Column-major iteration, 16 KiB stride   | Swap inner/outer loops (row-major)           |
| 2 | `route_slow`  |  69  | 18.8% | `format!` allocation per event          | Sum digits directly with `digit_byte_sum`    |
| 3 | `digest_slow` | 109  | 11.4% | Full `sort_unstable` × 3 per window     | `select_nth_unstable` on a reused buffer     |

All three fixes already exist as `*_fast` variants in the same file.
