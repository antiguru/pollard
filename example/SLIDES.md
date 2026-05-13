# pollard demo — 10-minute slide outline

Audience: R&D engineers. Assume they know what a profile is, may not have used samply/Firefox Profiler recently, have not seen pollard.

## 1. Title (15s)

* "pollard — let Claude read your profiles"
* What we'll show: same profile, two analysis workflows.

## 2. Profiling recap (90s)

Cover at the whiteboard / one slide:

* Sampling profilers: kernel/userspace stops the program every N µs, captures a stack trace, aggregates.
* Output is a tree of (function, sample-count). Hotspots = functions with many samples.
* What's "hot" depends on the event: cycles by default, but also cache-misses, branch-misses, instructions retired, page faults.
* Tools: `samply` (records), Firefox Profiler (visualizes), `perf` (Linux).

## 3. The demo program (60s)

Slide: the `log_p99` slow core (~20 lines, see `src/bin/log_p99.rs`).

* Stream of `(host, status, latency)` records.
* Per-bucket p99 emitted every `WINDOW` records.
* "Where is the time going?" Pause. Take a guess from the room.

## 4. Workflow A — without pollard (3 min)

Live demo or pre-recorded screencast:

* `samply record ./target/demo/log_p99 slow` — 3 s run.
* Firefox Profiler UI opens.
* Call Tree → Invert → top of list: `__rdl_alloc`, `<&str as Display>::fmt`, `core::fmt::write`.
* Right-click → Focus on function → caller is `alloc::fmt::format`.
* Walk back up to `run_slow` → the `format!` line.
* That's one of two defects. Switch back to non-inverted view to spot the second.
* Hover, click, scroll, repeat.

Talking point: the data is all there, the bottleneck is the human navigating the UI. For an unfamiliar codebase, you spend 5 minutes building a mental model of the call tree before you can act on it.

## 5. Workflow B — with pollard (3 min)

Same recording, but loaded into Claude:

```
load_profile slow.json.gz
top_functions limit=15
```

Claude reads the output and says (paraphrased):

> Two hotspots: `alloc::fmt::format` (28%) called from `run_slow`, and
> `core::slice::sort::sort_unstable` (22%) also from `run_slow`. Let me
> look at the source.

```
source_for_function function="log_p99::run_slow"
```

Claude proposes the fix — tuple key + `select_nth_unstable` — and you accept.

Record the fast profile, then:

```
compare_profiles base="slow" head="fast"
compare_functions base="slow" head="fast" function="run_slow"
```

The deltas show `alloc::fmt::format` gone, sort sample count down 95%, no regressions elsewhere. Validation in two tool calls.

## 6. Where it really pays off (90s)

Two extras if time:

* **Hardware counters.** Record `matmul slow` with `--rate cache-misses`. `top_functions event="cache-misses"` puts `matmul_slow` at the top with a much stronger signal than the cycles profile. The loop-order bug, in one tool call.
* **View presets.** Real Rust profiles are buried under tracing-subscriber `Layered` walls and tokio runtime frames. The `pollard:view-presets` skill ships regex sets that hide the noise. Same `top_functions` call, but now the user code surfaces.

## 7. Wrap (30s)

* pollard is an MCP server. Works with any client that speaks MCP (Claude Code, Claude Desktop, others).
* It doesn't replace the Firefox UI — it gives an LLM the same data, in a form it can reason over.
* Install: `/plugin install pollard@antiguru-tools` or `cargo install pollard`.
* Repo: github.com/antiguru/pollard

## Commands cheat sheet

```sh
# build
cargo build --profile demo -p pollard-demo

# record
samply record -o slow.json.gz --save-only ./target/demo/log_p99 slow
samply record -o fast.json.gz --save-only ./target/demo/log_p99 fast

# hardware-counter variant
samply record --rate cache-misses -o cm.json.gz --save-only ./target/demo/matmul slow
```

```text
# pollard tool calls (in Claude)
load_profile          slow.json.gz
top_functions         profile="slow" limit=15
source_for_function   profile="slow" function="log_p99::run_slow"
call_tree             profile="slow" focus="alloc::fmt::format"
load_profile          fast.json.gz
compare_profiles      base="slow" head="fast"
compare_functions     base="slow" head="fast" function="log_p99::run_slow"
```
