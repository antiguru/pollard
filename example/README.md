# pollard demo binaries

Three small Rust programs with planted performance defects, sized so each `slow` run takes 2-3 s — long enough for `samply record` to collect a usable profile, short enough to demo live.

Each binary has a `slow` and a `fast` mode that compute the same checksum / output, so you can record both and use pollard's `compare_profiles` and `compare_functions` to validate the fix.

## Build

```sh
cargo build --profile demo -p pollard-demo
```

The `demo` profile is `release` with `lto = false` and `debug = true` so function boundaries that matter to the profile (`format!`, `sort_unstable`, the matmul inner loop, the linear scan in `find`) survive into the symbol table.

Binaries land in `target/demo/`.

## Three defects

### `log_p99` — allocator pressure + redundant sort

Aggregates synthetic `(host, status, latency)` records, emits per-bucket p99 every `WINDOW` records.
Two layered defects:

1. Per-record `format!("{host}:{status}")` allocates a `String` key.
2. Per-emit, every bucket is cloned and `sort_unstable`'d to read a single percentile.

Fast mode uses a tuple key and `select_nth_unstable` in place.
Speedup: ~5×.

### `matmul` — cache misses

Textbook `ijk` dense f32 matmul.
The inner `k` loop strides through `b` one column at a time and misses cache on every load.

Fast mode swaps the inner two loops to `ikj`, same arithmetic, row-stride access on `b`.
Speedup: ~16×.

This is the binary to use if you want to showcase pollard's hardware-counter support — record with `perf record -e cache-misses`, convert with `samply import`, and pass `event: "cache-misses"` to `top_functions` / `call_tree`.

### `nested_join` — quadratic loop

Inner join between an event stream and a metadata table using `meta.iter().find(...)` per event.

Fast mode builds a `HashMap` index once.
Speedup: ~1200×.

The simplest of the three: the defect is visible on the slide. Use it as the warm-up.

## Profiling workflow without pollard

```sh
samply record ./target/demo/log_p99 slow
```

samply opens the Firefox Profiler UI in a browser tab. The human then:

1. Switches to **Call Tree** view.
2. Toggles **Invert** to see hotspots at the top.
3. Scans function names, recognises `__rdl_alloc`, `core::fmt::write`, `String::push_str`, etc.
4. Right-clicks a frame, picks **Focus on function**, reads the callers panel.
5. Walks back up to user code (`run_slow` → `format!` macro expansion → allocator).
6. Repeats for the second defect (`sort_unstable` inside the window loop).
7. Writes a fix, re-records, eyeballs the new profile to confirm.

Every step is manual UI navigation. The profile data is rich but the analysis is bottlenecked on the human clicking through it.

## Profiling workflow with pollard

```sh
samply record -o slow.json.gz --save-only ./target/demo/log_p99 slow
samply record -o fast.json.gz --save-only ./target/demo/log_p99 fast
```

Then in a Claude Code session with the `pollard` plugin installed:

```
load_profile  slow.json.gz                                       → profile id "slow"
top_functions profile="slow" limit=15                            → see hotspots
source_for_function profile="slow" function="log_p99::run_slow"  → read the hot code
                                                                   inline with sample counts
call_tree     profile="slow" focus="alloc::fmt::format"          → confirm format! is
                                                                   the alloc driver
load_profile  fast.json.gz                                       → profile id "fast"
compare_profiles base="slow" head="fast"                         → per-function deltas:
                                                                   what got cheaper, what
                                                                   regressed
compare_functions base="slow" head="fast" function="run_slow"    → line-level diff with
                                                                   sample counts
```

Each tool returns a compact, structured payload — no UI, no manual aggregation. The LLM has the same data the Firefox UI gets, but in a form it can reason over and quote back at you with line numbers.

For Rust-specific noise (tracing-subscriber walls, tokio internals, stdlib glue) apply the cookbook regex sets via `create_view` — see the `pollard:view-presets` skill.

## Hardware counters (matmul)

`samply record` only samples cycles. For hardware events, record with `perf` and import:

```sh
perf record -e cache-misses --call-graph dwarf -o slow.perf.data ./target/demo/matmul slow
samply import slow.perf.data -o slow.json.gz --save-only
```

Then:

```
top_functions profile="slow" event="cache-misses" limit=5
```

`matmul_slow` dominates the cache-misses event much more strongly than it dominates cycles. That is the loop-order bug in a single tool call.

The same workflow extends to other perf events — `page-faults` (see `page_fault` bin), `branch-misses`, `dTLB-load-misses`, etc.

## Suggested demo script

A 10-minute presentation outline lives in [`SLIDES.md`](./SLIDES.md).

## Recorded sessions

[`transcripts/`](./transcripts/) holds six side-by-side investigations — same user prompt ("I have program X and noticed it's slow"), once without pollard and once with. The `with-pollard` transcripts quote real tool output and reproduce from the binaries in this crate.
