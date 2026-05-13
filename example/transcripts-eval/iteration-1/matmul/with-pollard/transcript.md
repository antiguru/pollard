# matmul investigation

> I have a program `matmul` and noticed it's slow. Please profile and find the root cause.

## Load profile

`load_profile(path="/tmp/claude-1000/pf/matmul-slow.json.gz")`

* duration 3132 ms, 2841 samples in `matmul` (pid 16.1), 1 kHz cycles
* profile_id `5c955640`

## Top functions (slow)

`top_functions(profile_id="5c955640", process="pid:16.1")`

| rank | function | self_pct | self_samples |
|------|----------|----------|--------------|
| 1 | `matmul::main` | 99.7% | 2833 |
| 2 | `_GI___mmap64` | 0.2% | 6 |

Everything is in `main`; the hot loop is inlined into it. Re-run with `expand_inlines=true`:

| rank | function | self_pct |
|------|----------|----------|
| 1 | `matmul::matmul_slow` | 86.1% |
| 2 | `<usize as PartialOrd>::lt` | 13.3% (loop bound) |

99%+ of cycles in `matmul_slow`. Drill into source.

## Source attribution

`source_for_function(profile_id="5c955640", function="matmul::main", with_samples=true)`

```
line 21:  for k in 0..N {                            samples=376   13.24%
line 22:    s += a[i * N + k] * b[k * N + j];        samples=2436  85.77%
line 24:  c[i * N + j] = s;                          samples=9      0.32%
```

99.0% of self-time is inside the innermost `k` loop.

## Diagnosis

`matmul_slow` is the textbook **ijk** order:

```rust
for i in 0..N {
    for j in 0..N {
        let mut s = 0.0f32;
        for k in 0..N {
            s += a[i * N + k] * b[k * N + j];  // <-- 85.8% of samples
        }
        c[i * N + j] = s;
    }
}
```

With `N = 1280`, each inner-loop step advances `k` by 1.
The two loads per iteration are:

* `a[i*N + k]` — stride 4 bytes, sequential, perfect L1 hit pattern.
* `b[k*N + j]` — stride `N*4 = 5120` bytes per step. Each load lands on a different 64-byte cache line.

`b` is `N*N*4 = 6.25 MiB`, far larger than L1 (32 KiB) and L2 (typically 1 MiB).
Every `b[k*N+j]` load is an L1 miss, and most are L2 misses too, so the inner loop is memory-bound on the `b` column traversal.
Line 22 reads as 85.8% self because samples are attributed to the load that stalls.

## Fix

Swap the inner two loops (ijk → ikj).
Now the inner loop strides through `b` row-major (sequential, cache-line-friendly) and accumulates into `c` row-major:

```rust
fn matmul_fast(a: &[f32], b: &[f32], c: &mut [f32]) {
    for i in 0..N {
        for k in 0..N {
            let a_ik = a[i * N + k];
            for j in 0..N {
                c[i * N + j] += a_ik * b[k * N + j];
            }
        }
    }
}
```

`matmul_fast` already exists in the source — it just needs to be selected (`matmul fast`).
Same arithmetic, same trace, but the inner loop is now a unit-stride streaming pass over a row of `b` and a row of `c`, which the hardware prefetcher can saturate.

## Validation

Recorded `/tmp/claude-1000/pf/matmul-fast.json.gz` via `samply record -- matmul fast`.

Wall-clock from the program itself:

| mode | elapsed | GFLOPS |
|------|---------|--------|
| slow | 2754 ms | 1.52 |
| fast |  232 ms | 18.04 |

`compare_profiles(profile_id_a="5c955640", profile_id_b="a0893f9d", align_by="function", sort_by="delta_ms")`:

| function | a_self_ms | b_self_ms | delta_self_ms |
|----------|-----------|-----------|---------------|
| `matmul::main` | 2833 | 221 | **-2612** |

11.9x wall-clock speedup; trace value identical (`409706.0938`), so the fix is numerically equivalent.
