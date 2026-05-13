# nested_join performance investigation

> I have a program `nested_join` and noticed it's slow. Please profile and find the root cause.

## Load the slow profile

`load_profile(/tmp/claude-1000/pf/nested_join-slow.json.gz)`

* duration 3073 ms, 2893 samples on the `nested_join` thread, 1 kHz, cycles event.
* single thread, single process — no concurrency angle.

## Top-self functions (no inline expansion)

`top_functions(profile_id, process=pid:23.1, limit=10)`

* `nested_join::main` self 99.7% (2883 / 2893).
  Everything else is sub-1%.
  `join_slow` got inlined into `main`, so the work is hiding under it.

## Re-run with inline expansion

`top_functions(..., expand_inlines=true)`

| rank | function | self_pct | self_samples |
| --- | --- | --- | --- |
| 1 | `nested_join::join_slow::{{closure}}` | 50.7% | 1466 |
| 2 | `<slice::Iter<T> as Iterator>::find` | 45.0% | 1302 |
| 3 | `<slice::Iter<T> as Iterator>::next` | 3.7% | 106 |

96% of self time is inside `Iter::find` + the closure passed to `find`, with another 3.7% in `Iter::next`.
That is the textbook signature of a linear scan inside a loop.

## Line-level attribution

`source_for_function(profile_id, function="nested_join::main", with_samples=true)`

```
55:        for e in events {                                         0 samples
56:            if let Some(m) = meta.iter().find(|m| m.id == e.id) { 2874 samples (99.4%)
57:                acc = acc.wrapping_add(...);                       2 samples
```

99.4% of the program's time is one line: `meta.iter().find(|m| m.id == e.id)`.

## Diagnosis

`join_slow` does an O(N_EVENTS * N_META) nested scan:

```rust
fn join_slow(events: &[Event], meta: &[Meta]) -> u64 {
    let mut acc = 0u64;
    for e in events {
        if let Some(m) = meta.iter().find(|m| m.id == e.id) { // <-- 99.4%
            acc = acc.wrapping_add(u64::from(e.value) ^ u64::from(m.weight));
        }
    }
    acc
}
```

With `N_EVENTS = 500_000` and `N_META = 50_000` that's up to 2.5e10 comparisons.
The profile shows the hot path is exactly the linear-search `find`, not allocator / format / I/O noise.

## Fix

Build a `HashMap<u32, u32>` index over `meta` once, probe it per event — exactly what `join_fast` in the same file already does:

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

This drops the join to O(N_META) build + O(N_EVENTS) probes.
The slow `find` is replaced by `HashMap::get`.

## Validation

Re-ran the binary with both modes against identical input:

| mode | elapsed | acc |
| --- | --- | --- |
| slow | 2.520 s | `0x0003cfafbab6d880` |
| fast | 6.705 ms | `0x0003cfafbab6d880` |

Same `acc` — the fast version is correct.
Speedup ~376x, consistent with replacing O(events * meta) = 2.5e10 with O(events) = 5e5 work.

Fast-mode profile is 8 ms total / 16 samples — too short to do a meaningful `compare_profiles` against the 2893-sample slow profile, but the wall-clock and matching accumulator are sufficient evidence.

## Root cause

Linear scan `meta.iter().find(...)` inside the per-event loop in `join_slow` — line 56 of `example/src/bin/nested_join.rs` — accounts for 99.4% of runtime.
Switch the `match mode.as_str()` arm in `main` from `join_slow` to `join_fast` (or always use `join_fast`).
