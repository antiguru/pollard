# nested_join — with pollard

## User

> I have a program `nested_join` and noticed it's slow. Please profile and find the root cause.

## Assistant

Recording then loading into pollard.

```bash
$ samply record --save-only -o profiles/nested_join-slow.json.gz \
      ./target/demo/nested_join slow
mode=slow  events=500000  meta=50000  elapsed=3.024s  acc=0x0003cfafbab6d880
```

### Tool call: `load_profile`

```
path: profiles/nested_join-slow.json.gz
name: nested_join-slow
```

```json
{
  "profile_id": "1be46141",
  "description": {
    "duration_ms": 3024,
    "interval_ms": 1,
    "total_samples": 3025,
    "unsymbolicated_pct": 16.7,
    "processes": [{"name": "nested_join",
                   "threads": [{"name": "nested_join", "samples": 3025}]}]
  }
}
```

### Tool call: `top_functions limit=10`

```
profile_id: 1be46141
```

| rank | self_pct | total_pct | function |
|-----:|---------:|----------:|---|
| 1 | **100.0** | 100.0 | `nested_join::main` |
| 2 |   0.0   | 100.0 | `dyld` |
| 3 |   0.0   | 100.0 | `main` |
| 4 |   0.0   | 100.0 | `std::rt::lang_start::{{closure}}` |

`join_slow` inlined into `main`. Going to source.

### Tool call: `source_for_function function="nested_join::main" with_samples=true whole_file=true`

```text
line | self_pct | code
   53|     0.0  | fn join_slow(events: &[Event], meta: &[Meta]) -> u64 {
   54|     0.0  |     let mut acc = 0u64;
   55|     0.0  |     for e in events {
   56|    99.97 |         if let Some(m) = meta.iter().find(|m| m.id == e.id) {
   57|     0.0  |             acc = acc.wrapping_add(u64::from(e.value) ^ u64::from(m.weight));
   58|     0.0  |         }
   59|     0.0  |     }
   60|     0.0  |     acc
   61|     0.0  | }
```

### Diagnosis

99.97 % on **line 56**: `meta.iter().find(|m| m.id == e.id)`.

A linear scan over `meta` (50,000 entries) per event (500,000 events). Average 25K probes × 500K events ≈ 12.5G comparisons. Classic nested-loop join.

### Fix

Build a `HashMap<u32, u32>` index over `meta` once, then probe per event:

```rust
fn join_fast(events: &[Event], meta: &[Meta]) -> u64 {
    let index: HashMap<u32, u32> = meta.iter().map(|m| (m.id, m.weight)).collect();
    let mut acc = 0u64;
    for e in events {
        if let Some(&w) = index.get(&e.id) {
            acc = acc.wrapping_add(u64::from(e.value) ^ u64::from(w));
        }
    }
    acc
}
```

### After-fix profile

`nested_join fast` finishes in 2.4 ms (vs. 3024 ms slow) — too fast for the 1 ms sampler to catch more than a handful of samples. Speedup ≈ 1200×.

### Why pollard helped even on a trivial bug

This defect is visible from reading the source. The point of using a tool here isn't to *find* it — it's to *measure* it. 99.97 % on a single line is qualitatively different from "the linear scan looks slow":

* If `meta.iter().find` were 60 % and not 99.97 %, I'd be looking for a second hotspot.
* If the line attribution landed on `wrapping_add` instead of `find`, my hypothesis would be wrong and I'd revise it.
* The same workflow — `load_profile` → `top_functions` → `source_for_function` — scales to defects that aren't trivially visible (`log_p99`'s layered sort + alloc, `matmul`'s cache pattern). Using it on a trivial bug builds the habit of reaching for measurements before reaching for hypotheses.
