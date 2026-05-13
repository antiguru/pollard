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

I have a Firefox-format profile. To investigate I'd need to:

* Open it in the Firefox Profiler UI (`samply load /tmp/nested_join-slow.json.gz`), and ask you to read me the call tree, or
* Look at the source — `nested_join.rs` — and reason about what would be slow.

Reading the source for `join_slow`:

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

The `meta.iter().find(...)` is a linear scan, called once per event. 500K × 25K average probes = ~12.5G comparisons. That's the bug, and it's visible from reading.

But "I think it's the linear scan" without a profile is a hypothesis. I can't confirm what fraction of time is in the `find` vs. the `wrapping_add` vs. the iterator setup vs. generator code, without you opening the UI and reading off the call tree.

Could you open the profile UI and:

* Confirm the **Call Tree** top entry is `join_slow` or something inside it (closure for the `find` predicate, `Iterator::find`, etc).
* Tell me the self percentage — is it >90 %?

If yes, the fix is a `HashMap<u32, u32>` index built once outside the loop. If no, I need to keep digging — but for that, I need eyes on the profile.

## What this workflow misses

The case where the defect *is* obvious from reading the code — like this one — looks like a win for the no-tools workflow. It isn't:

* I never measured what `find` actually costs vs. what I assumed it costs. If the cardinality were lower and the linear scan were already fine, this advice would be a premature optimization.
* The profile collected richer signal (call stacks, allocations, scheduling) that's now thrown away.
* The handoff is still serial: you open the UI, you click through, you tell me.

For non-trivial defects (`log_p99`, `matmul`) this gap is much wider — but even on the trivial one, the lack of measurement turns "found the bug" into "guessed and got lucky".
