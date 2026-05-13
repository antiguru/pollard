# log_p99: investigation without pollard MCP

> I have a program `log_p99` and noticed it's slow.
> Please profile and find the root cause.

## 1. Inputs

* Profile: `/tmp/claude-1000/pf/log_p99-slow.json.gz` (samply Firefox JSON, cycles, 1 kHz, 1 thread).
* Binary: `/home/moritz/dev/repos/pollard/target/demo/log_p99` (Rust, debug info, `demo` profile).
* Source: `example/src/bin/log_p99.rs`.

The source's doc-comment lists two suspected defects.
I treat that as a hint only and verify both against the profile.

## 2. Top self-time, raw

Without pollard I parse the Firefox JSON directly.
The provided helper `top_funcs.py` shows function "names" are raw hex offsets — the profile is not symbolicated (`meta.symbolicated = false`, `nativeSymbols.length = 0`).

```
$ python3 /tmp/claude-1000/pf/top_funcs.py /tmp/claude-1000/pf/log_p99-slow.json.gz 10
   794   11.6%  0xed78
   619    9.1%  0xed61
   245    3.6%  0xed59
   179    2.6%  0xf423
   167    2.4%  0xea6b
   158    2.3%  0x168edd
   143    2.1%  0xed7f
   120    1.8%  0x1692c1
   113    1.7%  0xea54
   101    1.5%  0xea35
```

## 3. Symbolicate via addr2line

`libs[]` shows two libraries: the binary itself, and libc.
Frame addresses are lib-relative.
I split frames by lib using `funcTable.resource -> resourceTable.lib`, then `addr2line -f -C -p -i` for binary frames.

Binary `.text` ends near `0x4a62d`, so frames like `0x168edd` are in libc (the .text addresses inside the binary all sit below `0x4b000` according to `readelf -l`).

Resolving the hot offsets (inline-expanded) gives:

```
0xed78  core::slice::sort::unstable::quicksort::partition_lomuto_branchless_cyclic (sort)
0xed61  same
0xea6b  core::slice::sort::shared::smallsort::small_sort_network (sort)
0xf423  core::slice::sort::unstable::quicksort::quicksort (sort, recursion)
0xed7f  core::slice::cmp::eq (called from hashbrown find -> String == String)
0x168edd  libc __memcmp_avx2_movbe  (hashbrown key compare)
0x1692c1  libc __memcpy_avx_unaligned_erms  (allocator / vec growth)
```

So everything in the leaf landscape is sort, plus String hashing/eq, plus libc memcpy/malloc.

## 4. Group leaves into buckets

I walk every leaf address, expand inlines, and bucket by the innermost meaningful frame.
Script: `tmp_analysis/analyze.py`.

```
total leaf samples: 6836  (~6.8 s of on-CPU @ 1 kHz)

BUCKETS (leaf self-time)
   4820  70.5%  sort_unstable
    773  11.3%  libc (memcpy/memcmp/malloc)
    567   8.3%  format!  (fmt machinery)
    424   6.2%  HashMap probe (hashbrown)
    173   2.5%  user code body
     79   1.2%  other
```

## 5. Attribute back to the user callsite

The leaf-only breakdown still leaves libc/sort/fmt floating.
I re-attribute every sample to the deepest `run_slow` / `generate` callsite in its inlined call chain:

```
ATTRIBUTION BY DEEPEST USER CALLSITE
   4953  72.5%  run_slow:54   sort_unstable
    740  10.8%  run_slow:50   buckets.entry(key) -- String hash + eq
    697  10.2%  run_slow:49   format!("{}:{}")
    313   4.6%  run_slow:53   sorted = samples.clone()
     85   1.2%  generate:41   .collect()  (one-time setup, ignore)
```

The four `run_slow` lines account for ~98 % of the work.

## 6. Diagnose

### Defect 1: `sort_unstable` to read one element (~72.5 %)

```rust
51:        if (i + 1) % WINDOW == 0 {
52:            for samples in buckets.values() {
53:                let mut sorted = samples.clone();
54:                sorted.sort_unstable();
55:                checksum = checksum.wrapping_add(u64::from(sorted[sorted.len() * 99 / 100]));
56:            }
57:        }
```

Only the p99 element is read.
`sort_unstable` is O(n log n) when O(n) `select_nth_unstable` suffices.
With 100 windows × ~8 k buckets × thousands of samples each, this dominates.

### Defect 2: `samples.clone()` before sort (~4.6 %)

`buckets.values()` yields `&Vec<u32>` so a clone is required before any in-place mutation.
Pure overhead: 10 M `u32` copies per emit-window × 100 windows.
Iterating `buckets.values_mut()` avoids it.

### Defect 3: `format!`-ed `String` key (~21 % combined: 10.2 % fmt + 10.8 % hash + a share of libc memcmp/malloc)

```rust
49:        let key = format!("{}:{}", r.host, r.status);
50:        buckets.entry(key).or_default().push(r.latency_us);
```

* 10 M heap allocations (malloc / free pressure visible as `_int_malloc` and `__memcpy_avx_unaligned_erms` in libc).
* SipHash over a multi-byte UTF-8 string instead of a 24-bit integer.
* `__memcmp_avx2_movbe` on every probe collision (158 samples ≈ 2.3 % of total, all libc).

A `(u16, u8)` tuple key removes all three.

The `run_fast` function in the same file is precisely the corrected version: tuple key + `select_nth_unstable` + `values_mut()`.

## 7. Proposed fix

Replace `run_slow` with the existing `run_fast` semantics:

```rust
let mut buckets: HashMap<(u16, u8), Vec<u32>> =
    HashMap::with_capacity(usize::from(N_HOSTS) * 8);          // defect 3
for (i, r) in records.iter().enumerate() {
    buckets.entry((r.host, r.status))                          // defect 3
           .or_default()
           .push(r.latency_us);
    if (i + 1) % WINDOW == 0 {
        for samples in buckets.values_mut() {                  // defect 2
            let idx = samples.len() * 99 / 100;
            let (_, p99, _) = samples.select_nth_unstable(idx); // defect 1
            checksum = checksum.wrapping_add(u64::from(*p99));
        }
    }
}
```

Expected gain on this profile: the ~4.95 s in sort drops toward ~1 s (select_nth_unstable is O(n) average and avoids the clone), and ~1.4 s in fmt+hash+libc collapses with the tuple key.
Total wall ~6.8 s -> on the order of 1 s.

## 8. Gaps

* I had to write ~80 lines of Python plus repeated `addr2line` invocations to do what `pollard top_groups` / `pollard top_functions` / `pollard stacks_containing` would have done in three calls.
* I had to discover libc-vs-binary frame split manually from `libs[]` and `resourceTable`.
* Attribution by user callsite required a separate inline-chain pass per address; a real profiler tool would expose this directly via folded stacks.
