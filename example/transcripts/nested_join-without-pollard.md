# nested_join — without pollard

## User

> I have a program `nested_join` and noticed it's slow. Please profile and find the root cause.

## Assistant

I'll record a profile:

```bash
$ samply record --save-only -o /tmp/nested_join-slow.json.gz ./target/demo/nested_join slow
mode=slow  events=500000  meta=50000  elapsed=3.024s  acc=0x0003cfafbab6d880
```

3 s for 500K events × 50K meta entries. The cardinality alone suggests an O(n × m) join.

I have a Firefox-format profile. I can't query it directly, but a small python script over the stack/frame/func tables produces a top-N:

```bash
$ python3 transcripts/top_funcs.py /tmp/nested_join-slow.json.gz 5
   915  31.6%  0xc1f0
   837  28.9%  0xc1f9
   550  19.0%  0xc20c
   461  15.9%  0xc1f4
   106   3.7%  0xc1fe
```

`samply --save-only` doesn't symbolicate; resolve with `addr2line`:

```bash
$ addr2line -e ./target/demo/nested_join -f -C 0xc1f0 0xc1f9 0xc20c 0xc1f4 0xc1fe
nested_join::join_slow::{{closure}}
nested_join.rs:56
<core::slice::iter::Iter<T> as core::iter::Iterator>::find
.../core/src/slice/iter/macros.rs:349
nested_join::join_slow::{{closure}}
nested_join.rs:56
<core::slice::iter::Iter<T> as core::iter::Iterator>::find
…
<core::slice::iter::Iter<T> as core::iter::Iterator>::next
…
```

Three distinct symbols, 99 % of samples between them, all inside the inner `meta.iter().find(...)` machinery: the predicate closure on line 56, `Iter::find`, `Iter::next`. The linear scan is the hot spot.

Cross-check by reading the source — `nested_join.rs`:

```rust
fn join_slow(events: &[Event], meta: &[Meta]) -> u64 {
    let mut acc = 0u64;
    for e in events {
        if let Some(m) = meta.iter().find(|m| m.id == e.id) {
            acc = acc.wrapping_add(u64::from(e.value) ^ u64::from(m.weight));
        }
    }
    acc
}
```

The `meta.iter().find(...)` is a linear scan, called once per event. 500K × 25K average probes = ~12.5G comparisons. Source reading and the python+addr2line histogram point at the same line, which is reassuring — the profile is no longer a yes/no oracle but a primary source of evidence.

The fix is a `HashMap<u32, u32>` index built once outside the loop.

## What this workflow misses

* The python script tops out at leaf-frame aggregation. For this defect that's enough; for a bug where the hot leaf is `core::ptr::copy` called from three different sites, leaf attribution would tell me *what kind of work is hot* (copying) without telling me *which caller is driving it*. I'd need to extend the script to walk ancestor frames — another 30 lines, a second pass over the data.
* `addr2line` resolves one binary at a time. For profiles that cross into shared libraries (libc allocator, tokio runtime), I'd be running it against several files and stitching the results — or installing debuginfo for each. samply/pollard handle this for me.
* The profile has richer signal that the histogram throws away: call stacks, allocations, scheduling, hardware-counter markers when recorded. Each axis needs its own scripting pass.

For non-trivial defects (`log_p99`, `matmul`) this gap is much wider — but even on the trivial one, the lack of measurement turns "found the bug" into "guessed and got lucky".
