# Investigating nested_join slowness (without pollard)

> I have a program `nested_join` and noticed it's slow. Please profile and find the root cause.

## Step 1: Inspect the profile structure

The profile is a Firefox-format JSON. No pollard MCP available, so parse it with Python.

```
$ zcat /tmp/claude-1000/pf/nested_join-slow.json.gz | python3 -c "..."
threads: 2
  thread 0: name=samply samples=6
  thread 1: name=nested_join samples=2893
interval: 1.0 (ms; cycles event)
```

2893 samples on the workload thread. Frames have `address` but no resolved names in `funcTable` — samply emitted unsymbolicated names like `0xc1f0`, so I need `addr2line` against the binary.

## Step 2: Compute self / inclusive time

Walked `samples.stack` -> `stackTable.{prefix, frame}` -> `frameTable.func` -> `funcTable.name`.

```
=== TOP 5 by SELF time ===
   915 (31.6%)  0xc1f0
   837 (28.9%)  0xc1f9
   550 (19.0%)  0xc20c
   461 (15.9%)  0xc1f4
   106 ( 3.7%)  0xc1fe
```

Five tiny addresses inside one basic block account for ~99% of self samples. They all share the same stack prefix (inclusive 100%): `main` -> `<one function at 0xbba0..0xc47b>` -> these leaves.

## Step 3: Symbolicate

```
$ addr2line -e target/demo/nested_join -f -C -i 0xc1f0 0xc1f9 0xc20c 0xc1f4 0xc1fe
nested_join::join_slow::{{closure}}            example/src/bin/nested_join.rs:56
<core::slice::iter::Iter<T> as ...Iterator>::find   library/core/src/slice/iter/macros.rs:349
nested_join::join_slow                         example/src/bin/nested_join.rs:56
nested_join::main                              example/src/bin/nested_join.rs:82
... (same pattern for every hot address)
```

All five hot addresses inline to `nested_join.rs:56`, which is:

```rust
if let Some(m) = meta.iter().find(|m| m.id == e.id) {
```

## Step 4: Confirm via disassembly

```
$ objdump -d --start-address=0xc1c0 --stop-address=0xc220 target/demo/nested_join
c1f0: cmp  %r8d,-0x8(%rdi)   ; compare event.id vs meta[i-1].id
c1f4: je   c1c0              ; match -> accumulate
c1f6: cmp  %r8d,(%rdi)       ; meta[i].id
c1f9: je   c1c4
c1fb: test %r9,%r9           ; loop counter
c1fe: je   c1cd
c200: lea  0x18(%rdi),%r10
c204: add  $-0x18,%r9
c208: cmp  %r8d,0x8(%rdi)    ; meta[i+1].id
c20c: mov  %r10,%rdi
c20f: jne  c1f0               ; back-edge
```

A tight unrolled linear scan over `meta[]`, comparing each `meta.id` against the event id in `%r8d`. This is the `slice::Iter::find` from line 56.

## Diagnosis

`join_slow` does `meta.iter().find(...)` inside `for e in events`, i.e. a nested-loop join. With `N_EVENTS=500_000` and `N_META=50_000`, that's up to 2.5e10 comparisons. The profile shows ~99% of self time inside the inner scan — there is no other hotspot. Allocation, generation, and I/O do not appear in the top frames.

Source confirms intent (line 3): `Slow mode does a nested-loop scan: O(events * metadata).` The `join_fast` variant in the same file builds a `HashMap<u32, u32>` once and probes it — O(events + meta) expected, ~50x less work for these sizes.

## Fix

Replace the linear scan with a hash-indexed probe. The function already exists in the binary as `join_fast`:

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

Either invoke the binary with `fast` (`cargo run --release --bin nested_join -- fast`) or, if `join_slow` is what callers actually use, change its body to build a `HashMap` (or sort `meta` by `id` and binary-search) before the per-event lookup.

## Gaps

Without pollard I had to:

* Re-derive self/inclusive aggregation by walking `stackTable.prefix` in Python.
* Manually feed addresses to `addr2line` (one per hot frame; pollard would have done this on load and merged inline frames into a single function row).
* Disassemble a hand-picked window in `objdump` because no per-line attribution was available; pollard's `source_for_function` would have given line-attributed counts directly.

The conclusion is unambiguous either way: 99% of samples land on `nested_join.rs:56` inside an unrolled linear scan.
