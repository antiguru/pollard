# Iteration 1 — pollard vs no-pollard, 6 bins

Test prompt (identical for all eight agents): "I have a program X and noticed it's slow. Please profile and find the root cause."

Each row is one general-purpose subagent run with the same prompt and the same input profile, varying only the tool access.

## Results

| bin / config | duration_s | tool_uses | tokens | defects found | correct fix? |
|---|---:|---:|---:|---:|:-:|
| `matmul` / with-pollard       |  76.8 |  13 | 45 813 | 1/1 | yes (validated via `compare_profiles`) |
| `matmul` / without-pollard    | 188.7 |  26 | 50 583 | 1/1 | yes (no after-fix recording) |
| `page_fault` / with-pollard   |  90.9 |  16 | 47 713 | 1/1 | yes (validated via `compare_profiles`) |
| `page_fault` / without-pollard|  84.7 |  16 | 40 314 | 1/1 | yes (recorded fast.perf.data, diffed via `perf report`) |
| `log_p99` / with-pollard      |  92.5 |  15 | 50 005 | 3/3 | yes (validated via `compare_profiles`) |
| `log_p99` / without-pollard   | 428.0 |  55 | 92 249 | 3/3 | yes (no after-fix recording) |
| `nested_join` / with-pollard  |  59.8 |  11 | 42 756 | 1/1 | yes (re-ran fast for wall-clock cross-check) |
| `nested_join` / without-pollard | 102.2 | 14 | 42 744 | 1/1 | yes (no after-fix recording) |
| `pipeline` / with-pollard     |  93.5 |  14 | 49 420 | 3/3 | yes (validated via `compare_profiles`) |
| `pipeline` / without-pollard  | 165.9 |  18 | 60 254 | 3/3 | partial (recorded fast profile, didn't formally diff) |
| `pipeline_inlined` / with-pollard    |  73.4 | 12 | 54 027 | 3/3 | yes (validated via `compare_profiles`) |
| `pipeline_inlined` / without-pollard, hint    | 137.2 | 16 | 43 387 | 3/3 | no (proposed fixes from inline-chain attribution) |
| `pipeline_inlined` / without-pollard, no hint | 280.0 | 37 | 63 542 | 3/3 | no (proposed fixes from inline-chain attribution) |

## Aggregates

|                | with-pollard | without-pollard | ratio |
|----------------|---:|---:|---:|
| total duration | 487 s | 1107 s | **2.3×** |
| total tool calls | 81 | 145 | **1.8×** |
| total tokens | 289 734 | 329 531 | 1.1× |
| correct diagnoses | 6/6 | 6/6 | – |

## Per-bin differential

| bin | hotspots | stages inlined? | hint to without-pollard agent | duration ratio (without / with) |
|---|---:|:---:|:---|---:|
| `nested_join` | 1 | yes (into main) | none | 1.7× |
| `pipeline` | 3 | no (`#[inline(never)]`) | none | 1.8× |
| `pipeline_inlined` | 3 | **yes** (`#[inline(always)]`) | `addr2line -i` | 1.9× |
| `pipeline_inlined` | 3 | **yes** (`#[inline(always)]`) | **none** | **3.8×** |
| `matmul` | 1 | yes | none | 2.5× |
| `log_p99` | 2 | yes | none | **4.6×** |
| `page_fault` | 1 | yes | none | 0.9× |

## Observations

### When pollard's edge is biggest: `log_p99` (4.6×)

The leaf-frame histogram that without-pollard agents construct from the JSON dominates on a single-defect profile, but **dilutes when cost is spread across many leaves**. `log_p99` has two compounding defects (full sort + `format!`-ed String key); the format-driven cost spreads across `core::fmt::write`, `__rdl_alloc`, `hashbrown::rustc_entry`, `_memcmp_avx2_movbe`, `_memcpy_avx_unaligned_erms`. The without-pollard agent wrote two analysis passes — leaf-only histogram, then "attribution by deepest user callsite" walking inline chains — to surface defect #2. With-pollard hit it directly via `source_for_function` returning per-line samples on `run_slow`.

The without-pollard agent for `log_p99` spent 7 minutes, wrote ~80 lines of Python, ran addr2line repeatedly. The with-pollard agent for `log_p99` finished in 92 s with 15 tool calls.

### Surprise: `pipeline` (3 hotspots) was only 1.8×

The hypothesis going in was that three independent hotspots would widen pollard's edge further than `log_p99`'s two. It didn't. The without-pollard agent finished pipeline in 166 s — comparable to single-hotspot `matmul` (189 s).

The difference: `pipeline`'s three slow functions are marked `#[inline(never)]`, so they appear as distinct symbols in the unsymbolicated profile. The without-pollard agent's first `addr2line` pass on the top leaf addresses immediately surfaced `pipeline::route_slow`, `pipeline::digest_slow`, and `pipeline::rollup_slow` as three separate entries. From there it was three independent investigations, each with the structure of a single-hotspot defect.

### Counter-test 1: `pipeline_inlined` with `addr2line -i` hint — 1.9× (barely changed)

Built `pipeline_inlined` as a copy of `pipeline.rs` with `#[inline(never)]` swapped for `#[inline(always)]`. Same input, same checksum, identical work; profile shows 100 % of samples rolling up to `pipeline_inlined::main`, no stage symbols visible at function level.

Expectation: this should reproduce log_p99-style cost-spread, widening the gap. **First result, with `addr2line -i` hint in the prompt: gap stayed flat at 1.9× (137 s vs 73 s).** The without-pollard agent went straight to `addr2line -i` on every sampled address and bucketed by `(file:line)` rather than by leaf function. Three hot lines surfaced immediately.

### Counter-test 2: `pipeline_inlined` with the hint stripped — 3.8× (doubled)

The hint in counter-test 1 was the confound. Re-ran without-pollard with the hint removed (everything else identical: same profile, same multi-defect prompt, same tool restrictions). **Result: 280 s vs 73 s = 3.8× — roughly doubled the gap.** Tool calls also doubled (37 vs 16).

The hint-less agent eventually reached the same per-(file:line) attribution and the same three defects, but spent the extra 140 s exploring: leaf-only addr2line, looking at the funcTable shape, checking which libs/resources frames belonged to, before realizing `addr2line -fip` (inline-expanded) was what it needed. Once it had that axis, the analysis was clean.

### Refined takeaway

Pollard's edge isn't about hotspot count or inlining depth alone — it's about **how fast the agent reaches for the right axis to slice the profile on**.

* With pollard: per-line attribution is one tool call (`source_for_function`). Inline expansion is a boolean flag (`expand_inlines=true`). Default behavior matches what the investigation needs.
* Without pollard: the equivalent is a multi-step shell pipeline (`addr2line -i` over every sampled address, bucket by `(file, line)`, propagate inclusive samples up the inline chain). The pipeline is one workflow away — but the agent has to know to write it, and has to know `addr2line -i` (not just plain `addr2line`) is what makes inline expansion happen.

The gap distribution:
* **Cold-start, single-defect, no inlining-induced cost-spread** → ~1.7-2.5× (nested_join, matmul, pipeline)
* **Cold-start with inlining + cost-spread** → 3.8-4.6× (pipeline_inlined no-hint, log_p99)
* **Hinted** → ~1.9× regardless (pipeline_inlined with hint)
* **Native perf tooling competitive** → ≈1× (page_fault, where `perf report` does the heavy lifting)

### When pollard's edge is smallest: `page_fault` (0.9×)

The without-pollard `page_fault` agent skipped the JSON entirely and went straight to `perf report -i slow.perf.data` / `perf script` on the raw perf.data file. Native perf tooling is competitive when:
- There's one dominant hot stack (single defect, low-noise)
- The user has `perf.data` and not just the Firefox JSON
- The binary symbol resolution works inline (no cross-library frames)

In that regime pollard's edge collapses. The agent still self-reported the gap: "Without pollard's MCP I queried the profile via `perf report` / `perf script` and resolved addresses with `addr2line`. That worked here because one stack dominates; for a flatter profile I'd want a programmatic top-functions / call-tree query instead of eyeballing `perf report` output."

### Validation

Three of four with-pollard agents recorded a fast-mode profile and ran `compare_profiles` for validation. Only one without-pollard agent (page_fault) did the equivalent (`perf report` on fast.perf.data + manual comparison). The other three without-pollard agents proposed fixes without measured-validation — the friction of running a second analysis pipeline is enough to skip it.

This is a soft cost: the fix is still correct, but the user gets the answer with less evidence behind it.

### Reasoning depth

The without-pollard agents produced **richer narratives**, because they had to. Examples:
- `matmul`/without used `objdump` to verify the 4× unrolled k-loop and show the four B-column loads at strides `0x0/0x1400/0x2800/0x3c00`.
- `nested_join`/without disassembled the inner basic block to confirm the unrolled linear scan.
- `log_p99`/without bucketed leaf samples into "sort / libc / fmt / hashmap / user / other" categories.

This is a side-effect of having to manually construct what pollard provides as a single tool call. For pedagogical writeups (which is what we're using these for), the without-pollard narrative is more instructive about what's actually happening at the machine level. For "find the bug and fix it," it's pure overhead.

### Self-reported gaps (all four without-pollard agents)

* matmul: "I would have liked programmatic source-and-asm interleaving (pollard `source_for_function` / `asm_for_function`), and a quick way to see per-line self-time without hand-scripting addr2line."
* page_fault: "Without pollard's MCP I queried the profile via `perf report` / `perf script` and resolved addresses with `addr2line`. That worked here because one stack dominates."
* log_p99: "I had to write ~80 lines of Python plus repeated `addr2line` invocations to do what `pollard top_groups` / `pollard top_functions` / `pollard stacks_containing` would have done in three calls."
* nested_join: "Without pollard I had to: re-derive self/inclusive aggregation by walking `stackTable.prefix` in Python; manually feed addresses to `addr2line`; disassemble a hand-picked window in `objdump` because no per-line attribution was available."

All four converged on the same complaint: leaf-only addr2line is fine for one-defect profiles, but per-line attribution and inline-aware rollup are missing.

## Caveats

1. **Single run per pair** — no variance estimate. Differences this large (4.6× on log_p99) probably aren't noise, but the 0.9× and 1.7-1.9× ratios could easily flip on a re-run.
2. **Prompt confounds.** The `pipeline_inlined` / without-pollard agent was given an `addr2line -i` hint in its prompt. Without that hint the gap would likely be wider — closer to `log_p99`'s 4.6×, since the workaround is the slow part to discover. A cleaner test would prompt-match across all without-pollard agents.
3. **All defects are textbook** — single hot leaf or single hot stack. Real profiles have framework noise (tokio runtime, tracing-subscriber `Layered` walls, stdlib glue) that pollard's view-presets address and that the without-pollard path can't easily filter. None of the demo bins exercise this.
4. **Symbolication is the without-pollard agents' biggest cost.** samply's `--save-only` mode emits raw addresses; pollard symbolicates on `load_profile` via `wholesym`. If the input profile were already symbolicated (e.g. `perf report --no-children` output), the differential would shrink.
5. **The with-pollard agents had `source_for_function` + `expand_inlines`.** All multi-line attributions (matmul:22, log_p99:54, page_fault:117, pipeline_inlined:{140,69,109}) used these. The without-pollard equivalent (`addr2line -i` over sampled addresses, group by `(file:line)`) is one shell pipeline away — but the agent has to know to write it.
