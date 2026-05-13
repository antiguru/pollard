# matmul — with pollard

## User

> I have a program `matmul` and noticed it's slow. Please profile and find the root cause.

## Assistant

Recording then loading into pollard.

```bash
$ samply record --save-only -o profiles/matmul-slow.json.gz ./target/demo/matmul slow
mode=slow  N=1280  elapsed=2.047s  GFLOPS=2.05  trace=409706.0938
```

2 GFLOPS on a 1280³ multiply is roughly 15× below what a single core should do, so the budget is being burned somewhere obvious.

### Tool call: `load_profile`

```
path: profiles/matmul-slow.json.gz
name: matmul-slow
```

```json
{
  "profile_id": "67e16cb7",
  "description": {
    "duration_ms": 2050,
    "interval_ms": 1,
    "total_samples": 2051,
    "unsymbolicated_pct": 16.7,
    "processes": [{"name": "matmul",
                   "threads": [{"name": "matmul", "samples": 2051}]}]
  }
}
```

### Tool call: `top_functions limit=10`

```
profile_id: 67e16cb7
```

| rank | self_pct | total_pct | function |
|-----:|---------:|----------:|---|
| 1 | **100.0** | 100.0 | `matmul::main` |
| 2 |   0.0   | 100.0 | `dyld 0x1fdff` |
| 3 |   0.0   | 100.0 | `main` |
| 4 |   0.0   | 100.0 | `std::rt::lang_start::{{closure}}` |

Every cycle is in `matmul::main`. `matmul_slow` was inlined into `main` (small leaf function, no LTO needed). The interesting attribution is per-line.

### Tool call: `source_for_function function="matmul::main" with_samples=true whole_file=true`

```text
line | self_pct | code
   17|     0.0  | fn matmul_slow(a: &[f32], b: &[f32], c: &mut [f32]) {
   18|     0.0  |     for i in 0..N {
   19|     0.0  |         for j in 0..N {
   20|     0.0  |             let mut s = 0.0f32;
   21|    11.8  |             for k in 0..N {
   22|    87.5  |                 s += a[i * N + k] * b[k * N + j];
   23|     0.0  |             }
   24|     0.4  |             c[i * N + j] = s;
   25|     0.0  |         }
   26|     0.0  |     }
   27|     0.0  | }
```

### Diagnosis

87.5 % of samples land on **line 22**: `s += a[i*N+k] * b[k*N+j];`

In row-major layout `b[k*N+j]` for fixed `j` and varying `k` strides through `b` one column at a time — `4 × N = 5120` bytes between consecutive accesses, which is one cache line per load. Every inner-loop iteration eats an L1 miss.

`a[i*N+k]` is row-stride for fixed `i` and varying `k`, so loads from `a` are cheap. The bottleneck is `b`.

### Fix

Swap the inner two loops to `ikj`. Then `b[k*N+j]` is row-stride (in `j`) and `c[i*N+j]` accumulates row-stride too:

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

Same number of arithmetic operations. Different memory traffic.

### After-fix profile

`matmul fast` records as 132 samples / 131 ms vs. 2051 samples / 2050 ms slow. ≈16× speedup, **35 GFLOPS**, the inner loop has been auto-vectorized by LLVM now that the access pattern is amenable.

### Bonus: hardware-counter axis

If you'd recorded with `perf record -e cache-misses` and converted via `samply import` (samply's own recorder only samples cycles), `top_functions event="cache-misses"` would put `matmul_slow` even more decisively at the top — the cycles signal already nails it here because the loop is so memory-bound there's nothing else to spend time on. The hardware-counter axis pays off more when cycles and cache-misses disagree (e.g. a function that's hot but for branch-mispredict reasons, not memory).
