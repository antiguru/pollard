# log_p99 investigation

> I have a program `log_p99` and noticed it's slow. Please profile and find the root cause.

## Setup

* Slow profile: `/tmp/claude-1000/pf/log_p99-slow.json.gz` (samply, cycles, 1 kHz).
* Source: `/home/moritz/dev/repos/pollard/example/src/bin/log_p99.rs`.
* Binary: `/home/moritz/dev/repos/pollard/target/demo/log_p99`.

## Profile shape

`load_profile` reports the `log_p99` thread holds 6836 samples over 7339 ms (essentially CPU-bound, ~93% of wall on the single worker thread).

```
duration_ms=7339  interval_ms=1  samples=6836  unsymbolicated=0.0%
```

## Hot functions

`top_functions(process=log_p99, limit=20)`:

| rank | self% | total% | function |
|------|-------|--------|----------|
| 1 | 35.6 | 71.3 | `core::slice::sort::unstable::quicksort::quicksort` |
| 2 | 31.6 | 33.5 | `core::slice::sort::shared::smallsort::small_sort_network` |
| 3 | 4.0 | 4.0 | `_memcpy_avx_unaligned_erms` (libc) |
| 4 | 4.0 | 9.4 | `hashbrown::rustc_entry` |
| 6 | 2.8 | 2.8 | `_memcmp_avx2_movbe` (libc) |
| 8 | 2.5 | 8.8 | `core::fmt::write` |
| 11 | 1.3 | 10.1 | `alloc::fmt::format::format_inner` |

Sorting dominates (~71% inclusive) and the formatting / hashmap-string-key path accounts for another ~20%.

## Line attribution

`source_for_function(function="log_p99::main")` — `run_slow` was inlined into `main`, so source-line samples land here.
Lines inside `run_slow` (file `example/src/bin/log_p99.rs`):

| line | samples | self% | code |
|------|---------|-------|------|
| 49 | 697 | 10.2 | `let key = format!("{}:{}", r.host, r.status);` |
| 50 | 740 | 10.8 | `buckets.entry(key).or_default().push(r.latency_us);` |
| 53 | 313 | 4.6 | `let mut sorted = samples.clone();` |
| 54 | 4953 | 72.5 | `sorted.sort_unstable();` |

That's ~98% of CPU on four lines. Two distinct defects.

## Defect 1 — full sort to find one quantile (line 53–55, ~77% of CPU)

Each window-emit clones every bucket and runs a full `sort_unstable` just to read `sorted[len*99/100]`.
Full sort is `O(n log n)`; only the 99th-percentile element is needed, which is `O(n)` via `select_nth_unstable`.

Fix (in place, no clone):

```rust
for samples in buckets.values_mut() {
    let idx = samples.len() * 99 / 100;
    let (_, p99, _) = samples.select_nth_unstable(idx);
    checksum = checksum.wrapping_add(u64::from(*p99));
}
```

This drops both `quicksort` (4876 total samples / 71.3%) and the `samples.clone()` driving 273 self-samples of `_memcpy_avx_unaligned_erms` (line 53, 4.6%).

## Defect 2 — `format!`-ed `String` key per record (lines 49–50, ~21% of CPU)

Per-record `format!("{}:{}", host, status)` allocates a fresh `String`, runs through the `fmt` machinery (`core::fmt::write` + `format_inner` + `pad_integral` + two `Display::fmt` calls), and feeds a string-hashing / `memcmp`-comparing hashmap entry.
Evidence: 697 self-samples on line 49, plus the entry path on line 50 (740) attributing to `hashbrown::rustc_entry` (646 total samples), `core::fmt::write` (602 total), `_memcmp_avx2_movbe` (192 self).

`(host, status)` is a `(u16, u8)` — 3 bytes, `Copy`, hashable directly.

Fix:

```rust
let mut buckets: HashMap<(u16, u8), Vec<u32>> =
    HashMap::with_capacity(usize::from(N_HOSTS) * 8);
// ...
buckets.entry((r.host, r.status)).or_default().push(r.latency_us);
```

(Pre-sizing also avoids a few resize-grows; `N_HOSTS * 8` upper-bounds the cardinality.)

These are exactly the two fixes already encoded as `run_fast`.

## Validation — fast-mode comparison

Recorded `/home/moritz/dev/repos/pollard/target/demo/log_p99 fast` with samply (`d0c05d76`).
Wall time printed by the binary: **slow ≈ 7.30 s, fast = 1.88 s** (≈3.9× speedup).

`compare_profiles(a=slow, b=fast, sort_by=delta_ms, process=log_p99)` — top movers:

| function | a_self_ms | b_self_ms | delta_ms |
|----------|-----------|-----------|----------|
| `quicksort` | 2433 | 0 | -2433 |
| `small_sort_network` | 2160 | 0 | -2160 |
| `partition_at_index` (select_nth path) | 0 | 1203 | +1203 |
| `hashbrown::rustc_entry` (total) | 646 | 0 | -646 |
| `core::fmt::write` (total) | 602 | 0 | -602 |
| `_memcpy_avx_unaligned_erms` | 273 | 12 | -261 |
| `_memcmp_avx2_movbe` | 192 | 0 | -192 |

Both suspected hot spots are gone in fast mode; `partition_at_index` takes over the quantile work but at roughly half the cost of the previous sort (1203 ms vs 4876 ms total). Net thread CPU drops from 6835 ms to 1875 ms (-72.6%), matching the wall-time delta.

## Summary

* Two compounding defects, both in `run_slow`:
  1. `sorted.sort_unstable()` (line 54) — replace with `select_nth_unstable` and drop the `clone()` on line 53.
  2. `format!("{}:{}", host, status)` as a `HashMap<String, _>` key (lines 46, 49) — replace with `HashMap<(u16, u8), _>` keyed on the tuple, pre-sized.
* Fixes are exactly the `run_fast` body in the same file; applying them yields ~3.9× wall-time speedup, validated by `compare_profiles`.
