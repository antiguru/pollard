# pollard — design

`pollard` is a Model Context Protocol server that exposes Firefox-format
performance profiles (such as those produced by `samply record`) to AI
coding assistants. It runs as a local stdio MCP server, parses and
symbolicates a profile once, and answers structured queries — top
functions, pruned call trees, stacks containing a frame, source and
disassembly with per-line/per-instruction sample counts — over the loaded
profile.

The tool's name comes from the forestry technique of *pollarding*: cutting
back a tree's upper branches to encourage dense, manageable regrowth. The
core design problem here is exactly that — pruning huge call trees down to
what an LLM can hold and reason about.

## Goals

- Make `samply` profile data accessible to AI coding assistants (primarily
  Claude Code) for both human-driven and agent-driven workflows.
- Support four query shapes: top-down summary, drill-down through the call
  tree, stacks containing a function, and source-level attribution.
- Ship as a separate, third-party tool that depends only on samply's
  published library crates — no upstream changes required.

## Non-goals (v1)

- Recording as an MCP tool (`record`). The agent uses Bash to run
  `samply record --save-only` itself, then calls `load_profile`.
- Profile-format conversion (perf.data, ETW, etc.). Input must already be a
  Firefox-format profile JSON. samply handles those conversions; we don't.
- A human-facing CLI subcommand. The MCP server is the only surface.
- Authentication / remote access. Local stdio only.

## Shipped beyond the original v1

These were called out as non-goals in the first cut of this spec but
landed shortly after. Documented here so future readers don't think the
spec lags reality:

- **Marker / event analysis.** `top_functions`, `call_tree`, and
  `compare_profiles` accept an optional `event` argument naming a marker
  with a `cause.stack` payload (e.g. `cache-misses`, `branch-misses`,
  `instructions`). See *Event sources*. `top_groups` aggregates samples
  only and ignores `event`.
- **Cross-profile diff.** Shipped as
  `compare_profiles(profile_id_a, profile_id_b, ...)`. Same filter and
  `event` rules apply to both sides; events are resolved against
  profile A.

## Architecture

A single Rust binary, `pollard`, in its own repository. It stands up an
MCP server over stdio and holds an in-memory map of loaded profiles for
the lifetime of the process.

Internally, two modules:

- A **query module** that aggregates over a parsed Firefox profile. Pure
  Rust, deterministic, library-shaped. This is where the product lives.
- An **MCP layer** that registers tools, parses arguments, dispatches to
  the query module, and serializes responses. Thin.

Profile parsing and symbolication are delegated to samply's published
library crates (`fxprof-processed-profile`, `wholesym`, `samply-symbols`,
`samply-api`). Recording is delegated to the `samply` binary on `PATH`
when the agent chooses to call it via Bash; pollard never spawns it.

### Process model

- Stdio MCP server. Started by the MCP host (Claude Code), one server
  process per session.
- Holds `HashMap<ProfileId, Arc<ProfileSession>>`. Queries take a shared
  reference to a `ProfileSession` and run concurrently against the same
  loaded profile.
- Profile IDs are short stable strings derived from the absolute path
  (the first 8 hex chars of a hash, with an optional human-supplied
  `name`).
- Resident memory per loaded profile depends almost entirely on how much
  debuginfo wholesym pulls in, not on file size or sample count. See
  *Memory management* for measured ranges.

### Dependencies

All published, all on crates.io as of 2026-04-28:

- `rmcp` — Rust MCP SDK; stdio server, tool wiring.
- `tokio` — async runtime.
- `wholesym` — symbolication.
- `fxprof-processed-profile` — Firefox-format profile types.
- `samply-symbols` — debug-info parsing (transitive but worth pinning).
- `samply-api` — `/source/v1` and `/asm/v1` endpoints (for
  `source_for_function` and `asm_for_function`).
- `serde`, `serde_json` — JSON.
- `flate2` (or `zlib-rs`) — read `.json.gz`.
- `regex` — function-name matching with the `re:` prefix.
- `insta` (dev) — snapshot testing.

### Failure surface vs. samply

`pollard` does not ship with `samply` and does not require it for the
file-loading workflow. It only needs the `samply` binary on `PATH` if the
agent itself wants to record (via Bash, not via an MCP tool). The
`load_profile` flow works on any Firefox-format profile JSON regardless of
origin.

## MCP tool surface (14 tools)

Tools are organized into four groups: session management, describe, query,
and drill-down.

### Session management

#### `load_profile(path, name?) -> ProfileMetadata`

Parse and symbolicate the profile at `path`. Blocks until ready (or
fails). Returns a `ProfileMetadata` carrying the profile id and basic
shape.

- `path: string` — absolute or relative path to a `.json` or `.json.gz`
  Firefox-format profile.
- `name: string?` — optional human-readable label. Defaults to the file
  basename without extension.

Idempotent: loading the same `path` returns the existing `profile_id`.
Concurrent calls for the same path deduplicate — the second awaits the
first.

#### `unload_profile(profile_id) -> {}`

Free the memory held by a loaded profile. Returns an empty payload on
success, `profile_not_found` if the id is unknown.

#### `list_profiles() -> [ProfileMetadata]`

What is currently loaded.

#### `summary(profile_id, thread?, process?, time_range?) -> Summary`

One-shot orientation: profile shape (duration, sample rate, time range,
unsymbolicated bracket), top processes and threads by sample count,
top modules, and top functions by self / total time. Accepts the
standard filter args — passing `process` / `thread` / `time_range`
re-scopes every sample count to that slice without changing the
response shape. Aggregates the samples track only; `event` has no
effect.

Use this first instead of chaining `describe_profile` + `top_functions`;
it returns a denser bird's-eye view in one round-trip. Categorized as
session management for ergonomics, but doubles as a query entry point.

### Describe

#### `describe_profile(profile_id, top_n?) -> ProfileMetadata`

Returns top processes and threads by sample count (with totals and
omitted-entry counts), total samples, total duration, sample rate, and
`unsymbolicated_pct` (the fraction of frames that did not symbolicate).
`top_n` widens the per-call window when the default is too narrow for
a many-thread / many-process profile.

### Query

#### `top_functions(profile_id, thread?, process?, time_range?, event?, sort_by="self", limit=30, filter?) -> TopFunctions`

Flat top-N functions.

- `thread`, `process`, `time_range`: see *Filter grammar*.
- `event: string?` — see *Event sources*. Defaults to the samples track.
- `sort_by: "self" | "total" | "descendants"` — default `"self"`.
  Whether to rank by self-samples (functions where time is *actually
  spent*), total-samples (functions that *contain* the most time,
  including their callees), or descendants-only (total minus self).
  Typo'd values return `invalid_value`.
- `limit: number` — default 30.
- `filter: string?` — see *Function matching*. Restricts the result set to
  functions matching the filter. The only function-pattern arg that
  treats an empty string as "no filter".

#### `call_tree(profile_id, thread?, process?, time_range?, event?, inverted?, root_function?, paths_to?, min_pct=1.0, max_depth=8, max_breadth=5) -> CallTree`

Pruned hierarchical call tree.

- `thread`, `process`, `time_range`: see *Filter grammar*.
- `event: string?` — see *Event sources*. Defaults to the samples track.
- `inverted: bool` — default `false`.
  - `false`: rooted at top-of-stack frames (typically thread entry /
    `main`). Children are callees. "Where does my program spend its
    time, broken down by what called what?"
  - `true`: rooted at bottom-of-stack frames (leaf-most frames where time
    is actually spent). Children are callers. "What was running when each
    leaf function was hot, and who called into it?"
- `root_function: string?` — restrict the tree to subtrees rooted at
  frames matching this function (see *Function matching*). Combine with
  `inverted` for "what calls X" (`inverted=true, root_function=X`) or
  "what does X call" (`inverted=false, root_function=X`).
- `paths_to: string?` — prune the tree to only paths that reach a frame
  matching this function. Useful for "show me how we got into
  `lock_acquire`." Different from `root_function`: keeps the original
  root and prunes branches that don't reach the target.
- Pruning knobs (`min_pct`, `max_depth`, `max_breadth`) bound the output
  size. See *Pruning policy* for what the defaults guarantee.

#### `stacks_containing(profile_id, function, thread?, process?, time_range?, limit=20) -> Stacks`

Distinct full stacks that include a frame matching `function`. Ordered by
sample count. Each stack lists frames root-to-leaf with the matched frame
flagged. Different from an inverted call tree because the leaf is the
deepest frame in the stack (often a syscall), not the matched function.

#### `top_groups(profile_id, thread?, process?, time_range?, group_by="function", sort_by="self", limit=30, filter?, directory_depth?, expand_inlines?) -> TopGroups`

Flat top-N aggregation under a caller-chosen group key. Use first when
one binary or directory dominates and you want to know *which* before
drilling into functions.

- `group_by: "function" | "module" | "file" | "directory"` — default
  `"function"` (matches `top_functions` modulo the module-disambiguation
  column). Typo'd values return `invalid_value`.
- `sort_by: "self" | "total" | "descendants"` — same semantics as
  `top_functions`.
- `directory_depth: int?` — only meaningful for `group_by="directory"`.
  Truncates the path to the first N components.
- `filter: string?` — frame-level filter applied *before* grouping (a
  `group_by="module"` query with `filter="hot"` only counts frames whose
  function names match `"hot"`).
- Aggregates samples only; `event` is not accepted.

#### `folded_stacks(profile_id, thread?, process?, time_range?) -> FoldedStacks`

Flame-graph fold output (`frame;frame;... count\n`). Intended as input to
external visualizers (FlameGraph.pl, speedscope). Not designed for the
LLM to read directly — `call_tree` and `stacks_containing` are the
LLM-shaped views.

#### `compare_profiles(profile_id_a, profile_id_b, thread?, process?, time_range?, event?, filter?, align_by="function_and_module", sort_by="delta", limit=30, min_delta_pct?, expand_inlines?) -> Comparison`

Per-function diff between two loaded profiles. Each side aggregates
self-samples under the supplied filters; output ranks rows by the
delta (largest absolute change first by default).

- Both profiles must already be loaded; the same filter, `event`, and
  matching rules apply to both sides.
- `align_by: "function_and_module" | "function"` — default
  `"function_and_module"`, which strips cargo's 16-hex build-hash suffix
  before keying so two builds of the same binary still align.
  `"function"` drops module from the key entirely (cross-binary
  comparisons). Typo'd values return `invalid_value`.
- `sort_by: "delta" | "delta_ms" | "a" | "b"` — default `"delta"`
  (`|b_self_pct − a_self_pct|`). `"delta_ms"` is robust to changes in
  total profile duration.
- `min_delta_pct: number?` — drop rows whose absolute self-pct delta is
  below this. Filters out rounding noise on the long tail.
- Output rows omit `*_ms` columns when `event` is not time-shaped.

### Drill-down

#### `source_for_function(profile_id, function, module?, with_samples=true, whole_file=false, expand_inlines=false) -> SourceListing`

Source code for the function, with per-line sample counts merged in.
Implementation reuses samply-api's `/source/v1` to fetch file content and
attributes samples per line from the symbolicated frames in the loaded
profile.

By default returns only the function's line range plus 5 lines of context
above/below. `whole_file=true` returns the entire file (use when the
function spans the bulk of the file or when broader context is needed).

When `expand_inlines=true` the function matcher also considers DWARF
inline frames, so callers can ask for the source of an inlined callee
(e.g. `core::iter::Sum::sum`) instead of only the enclosing native
function.

#### `asm_for_function(profile_id, function, module?, with_samples=true) -> AsmListing`

Disassembly for the function with per-instruction sample counts. Reuses
samply-api's `/asm/v1`.

#### `address_to_function(profile_id, address, module?) -> FrameInfo`

Resolve a single library-relative address to a function name (and
file/line where available). Diagnostic for profiles with unresolved hex
offsets — wraps the same wholesym lookup pollard runs on load. `module`
is an optional substring matched against `lib.name`, `lib.debug_name`,
`lib.path`, or `lib.debug_path`; without it, every loaded library is
tried in order until one resolves.

#### `compare_functions(profile_id_a, function_a, function_b, profile_id_b?, module_a?, module_b?, with_samples=true) -> CompareFunctionsOutput`

Side-by-side asm diff of two functions, with per-instruction sample
counts on both sides.
Internally calls `asm_for_function` once per side and aligns the two
streams row-by-row using LCS over a normalized instruction key
(registers collapsed to `R`, numeric immediates to `IMM`,
mnemonic + operand shape preserved).
The displayed asm text is unchanged — normalization is alignment-only.

`profile_id_b` defaults to `profile_id_a`, so the natural
"why is `simd_rows_1st` faster than `simd_cols_1st`" workflow only
needs one profile.
Pass a different `profile_id_b` to read a before/after refactor across
two recordings.

Without normalization, register renames and differing displacements
(`xmm0`/`xmm1`, `rdi+rcx*8-0x38` / `rdi+rcx*1-0x6000`) would split
nominally-equal instructions onto separate rows and the per-row sample
columns would no longer line up — defeating the whole point.
LCS gap-fills with `Option<.>` fields (`only_a` / `only_b` per
position) so the surplus instructions on the longer side remain
visible at their original offsets.

The two sides must agree on `arch`; a mismatch returns `Internal` rather
than producing a meaningless alignment.

## Data shapes and pruning policy

Pruning is the central design decision. Without bounds, a `call_tree` for
a 30-second 1 kHz profile is tens of thousands of nodes — useless to an
LLM. The defaults below typically keep `call_tree` output under a few
thousand tokens of JSON for realistic inputs.

### Common conventions

Every frame in every output carries both `function` and `module` —
`malloc` is ambiguous without `libc.so.6`, and `main` is meaningless
without its containing binary. Sample counts and percentages both appear
on every node. Percentages alone lose precision at small totals; counts
alone are noisy across thread/process boundaries.

#### Function matching

Every parameter that takes a function name (`filter`, `function`,
`root_function`, `paths_to`) follows the same matching rules:

- **Default: substring match** on the demangled function name. `"malloc"`
  matches `"_int_malloc"` and `"je_malloc"`.
- **Regex: prefix with `re:`.** `"re:^memcpy_"` matches functions whose
  name starts with `memcpy_`.
- Matching is case-sensitive.
- A `module` parameter (where present) further constrains by the binary
  name. Without `module`, matches across all loaded modules.
- Empty / blank patterns are rejected for required pattern args
  (`function`) and for narrowing args that would otherwise silently
  match every frame (`root_function`, `paths_to`). Optional `filter` is
  the one exception: empty means "no filter". This split lets the LLM
  pass through a "leave blank to keep results unfiltered" UX without
  also letting it accidentally match-all on a drill-down.

#### Filter grammar

`thread`, `process`, and `time_range` apply uniformly across every query
tool. They are gating filters, not hints — samples outside the filter
are not aggregated.

- `process: string?` — process name (substring match), `pid:NNN` for an
  exact pid, or `pid:NNN.M` to pin to one of samply's `.M`
  sub-processes that share an OS pid (samply emits `pid 1234.0`,
  `1234.1`, ...). Bare-name matches aggregate across every matching
  process. Malformed `pid:` prefixes (e.g. `pid:abc`, `pid:1.2.3`)
  hard-error rather than silently falling back to name matching.
- `thread: string?` — thread name (substring match) or `tid:NNN` for an
  exact tid. Same malformed-prefix rule as `process`.
- `time_range: [number, number]?` — `[start_ms, end_ms]` from profile
  start. Out-of-bounds windows return zero samples; that is the answer,
  not a clamp + warning. Caller is expected to size the window against
  `describe_profile.duration_ms`.

#### Event sources

By default, queries aggregate the samples track (CPU cycles in samply's
perf recorder). Profiles also carry markers for hardware counters such
as `cache-misses`, `branch-misses`, `instructions`. Pass
`event="cache-misses"` (etc.) to `top_functions`, `call_tree`, or
`compare_profiles` to aggregate that counter instead.

Two error shapes distinguish the failure modes:

- **Unknown event** — message lists every stack-bearing marker name in
  the profile as suggestions. The LLM is meant to retry with one.
- **Stackless marker** — the marker name exists but its entries carry
  no `cause.stack` payload (e.g. text-only `mmap` annotations). The
  message names the matched marker and explains the missing
  `cause.stack`. Distinct from unknown-event so the caller can tell "I
  asked for the wrong name" apart from "this name exists but isn't
  aggregatable."

`top_groups` and `folded_stacks` aggregate samples only and ignore
`event`.

Percentages in source listings (`self_pct`) are denominated against
the function's own sample count, not the whole profile — these answer
"which lines in this function are hot." Percentages elsewhere (`self_pct`,
`total_pct`) are denominated against the relevant aggregation scope
(profile-wide, or thread-restricted when a `thread` filter is supplied).

Tie-breaking rules (required for snapshot-test stability):

- `top_functions`: sort by `self_samples` desc, then `function` asc, then
  `module` asc.
- `call_tree` children: sort by `total_pct` desc, then `function` asc.
- `stacks_containing`: sort by `samples` desc, then frame chain
  lexicographically.

### `top_functions` output

```json
{
  "thread": "GeckoMain (tid 12345)",
  "process": "firefox (pid 9876)",
  "total_samples": 30000,
  "filter": null,
  "inverted": false,
  "functions": [
    {
      "rank": 1, "function": "memcpy", "module": "libc.so.6",
      "self_samples": 2460, "self_pct": 8.2,
      "total_samples": 2460, "total_pct": 8.2
    }
  ]
}
```

### `call_tree` output

```json
{
  "thread": "GeckoMain (tid 12345)",
  "total_samples": 30000,
  "pruning": {"min_pct": 1.0, "max_depth": 8, "max_breadth": 5},
  "tree": {
    "function": "main", "module": "myapp",
    "self_samples": 30, "self_pct": 0.1,
    "total_samples": 28500, "total_pct": 95.0,
    "children": [
      {
        "function": "process_request", "module": "myapp",
        "self_samples": 600, "self_pct": 2.0,
        "total_samples": 24000, "total_pct": 80.0,
        "children": [...]
      },
      {"_omitted": {"count": 12, "combined_pct": 0.8}}
    ]
  }
}
```

Pruning rules, all default-on, all overridable:

- **`min_pct=1.0`** — drop subtrees whose `total_pct` is below this.
  Compress dropped siblings into a single `_omitted` summary node carrying
  the count and combined percentage.
- **`max_depth=8`** — cap depth. Beyond it, replace the subtree with
  `{"_truncated": {"deepest_descendant_pct": 3.2}}` so the LLM knows there
  is more and can raise the bound.
- **`max_breadth=5`** — cap children per node. Excess collapses into the
  same `_omitted` shape.
- **Linear-chain compression** — collapse runs of single-child nodes
  (`a → b → c` where each has only one significant child) into one node
  with `"chain": ["b", "c"]`. Often halves depth without information
  loss.

The `_omitted` and `_truncated` markers are explicit because they let the
LLM detect that something was hidden and re-query with looser bounds.

### `stacks_containing` output

```json
{
  "function_filter": "malloc",
  "matched_frame_samples": 12450,
  "matched_pct": 41.5,
  "unique_stacks_total": 87,
  "stacks_returned": 20,
  "stacks": [
    {
      "samples": 3200, "pct": 10.7,
      "frames": [
        {"function": "main",            "module": "myapp"},
        {"function": "process_request", "module": "myapp"},
        {"function": "alloc_buffer",    "module": "myapp"},
        {"function": "malloc",          "module": "libc.so.6", "matched": true},
        {"function": "_int_malloc",     "module": "libc.so.6"},
        {"function": "brk",             "module": "libc.so.6"}
      ]
    }
  ]
}
```

Frames are root-to-leaf. The matched frame carries `"matched": true`.

### `source_for_function` output

```json
{
  "function": "process_request", "module": "myapp",
  "file": "src/server.rs", "language": "rust",
  "total_function_samples": 1500,
  "line_range": [40, 78],
  "lines": [
    {"line": 40, "samples": 0, "code": "fn process_request(req: Request) {"},
    {"line": 44, "samples": 800, "self_pct": 53.3, "code": "    validate(&parsed);"}
  ]
}
```

By default, lines outside `line_range ± 5` are dropped. `whole_file=true`
returns every line.

### `compare_functions` output

```json
{
  "function_a": "sum_rows", "module_a": "rowcol",
  "function_b": "sum_cols", "module_b": "rowcol",
  "arch": "x86_64",
  "total_samples_a": 1754,
  "total_samples_b": 14639,
  "rows": [
    {"offset_a": 32, "asm_a": "addsd xmm0, qword [rdi + rcx * 8 - 0x38]", "samples_a": 0,
     "offset_b": 32, "asm_b": "addsd xmm0, qword [rdi + rcx * 1 - 0x6000]", "samples_b": 0},
    {"offset_a": 38, "asm_a": "addsd xmm0, qword [rdi + rcx * 8 - 0x30]", "samples_a": 228,
     "offset_b": 41, "asm_b": "addsd xmm0, qword [rdi + rcx * 1 - 0x4000]", "samples_b": 3543},
    {"offset_a": 56, "asm_a": "addsd xmm0, qword [rdi + rcx * 8 - 0x18]", "samples_a": 204}
  ]
}
```

Each row carries the asm text and per-instruction sample count for one
or both sides.
A row with only `_a` fields is an instruction present on side A with no
LCS counterpart on side B (and vice versa).
Matched rows have both — that's where the per-row sample columns line
up for direct comparison.
Total-sample columns at the top let the caller eyeball the wall-time
ratio between the two functions before reading the per-instruction
detail.

### `describe_profile` output

```json
{
  "profile_id": "a1b2c3d4",
  "name": "myapp-trace",
  "path": "/tmp/myapp.json.gz",
  "duration_ms": 30000,
  "interval_ms": 1.0,
  "sample_rate_hz": 1000,
  "total_samples": 750000,
  "unsymbolicated_pct": 0.4,
  "processes": [
    {
      "pid": 9876, "name": "myapp", "thread_count": 8,
      "threads": [
        {"tid": 12345, "name": "main", "samples": 30000, "duration_ms": 30000}
      ]
    }
  ]
}
```

## Error handling

Errors come back as MCP tool errors with a structured payload:

```json
{"error": "function_not_found", "function": "memcyp",
 "nearest_matches": ["memcpy", "memcpyc", "mempcpy"]}
```

The principle is *prefer warning + recovery over hard failure* wherever
the LLM has enough information to retry. Hard failures only when the
input is fundamentally invalid.

| Error code | When | Payload |
|---|---|---|
| `file_not_found` | `load_profile` path doesn't exist | `path` |
| `not_a_profile` | File exists but doesn't parse | `path`, `details` |
| `unsupported_profile_format` | Recognized but unsupported version | `path`, `version` |
| `symbolication_partial` | Non-fatal | Surfaced via `unsymbolicated_pct`, never as a hard failure |
| `function_not_found` | No frames match | `function`, `nearest_matches` (top 5 by string distance) |
| `function_ambiguous` | Multiple distinct functions match | `function`, `candidates: [{function, module}]` |
| `thread_not_found` | Thread filter doesn't match | `thread`, `available_threads: [{tid, name}]` |
| `process_not_found` | Process filter doesn't match | `process`, `available_processes: [...]` |
| `out_of_bounds` | Reserved | Not currently raised: out-of-bounds `time_range` returns zero samples instead. The caller is expected to size the window against `describe_profile.duration_ms`. |
| `invalid_value` | Typo'd enum value (e.g. `sort_by="seff"`, `group_by="moduel"`, `align="endian"`) | `field`, `value`, `accepted: [...]` listing the legal alternatives |
| `profile_not_found` | Unknown id, never loaded | `profile_id` |
| `profile_evicted` | Id was loaded but later evicted under memory pressure | `profile_id`, `original_path` so the LLM can `load_profile` it again |
| `internal` (unknown event) | `event=<name>` matches no marker in the profile | message lists every stack-bearing marker name as suggestions |
| `internal` (stackless marker) | `event=<name>` matches a marker but every entry has `cause.stack=null` | distinct from unknown-event; message names the matched marker and the stack-bearing alternatives |

## Memory management

Two layered mechanisms:

1. **`unload_profile(profile_id)`** is the primary mechanism. The LLM
   unloads profiles it no longer needs. Tool descriptions hint at this
   ("call when done with this profile to free memory").
2. **LRU eviction** as a safety net. The server holds at most N profiles
   concurrently, default 4, configurable via the `POLLARD_MAX_PROFILES`
   environment variable. When `load_profile` would exceed N, the
   least-recently-touched profile is evicted. Eviction is logged to the
   server's stderr.

Tool calls referencing an evicted id return `profile_evicted` with the
original path, letting the LLM re-load on demand. Re-loading triggers
re-symbolication, which is slow; the LRU bound is set generously enough
that this is rare in practice.

### Measured resident sizes

Resident memory is dominated by symbolication scaffolding (decompressed
DWARF sections + addr2line line tables held inside wholesym), not by
sample count or compressed file size. Reference numbers from
`examples/measure_rss`:

| Profile | File | Threads | Samples | RSS Δ steady | VmHWM peak |
|---|---|---|---|---|---|
| pager (small Rust) | 0.3 MiB | 4 | 16k | 73 MiB | 159 MiB |
| `perf record -a` 30 s | 2.7 MiB | 9400 | 39k | 159 MiB | 319 MiB |
| materialize (rich debuginfo) | 2.8 MiB | 1107 | 78k | 1032 MiB | 3939 MiB |

Two implications:

- The default `POLLARD_MAX_PROFILES=4` is sized for typical samply
  output (~150 MiB each) and produces a worst-case ~1.5 GiB resident.
  On Rust binaries with rich debuginfo, four loaded profiles can
  conservatively reach 4 GiB resident plus several GiB of transient
  peak during a fresh load. Operators of memory-constrained hosts
  should cap N lower.
- Peak/steady ratio is ~2× for typical workloads, ~3.8× for
  debuginfo-heavy ones — partly per-library DWARF decompression
  buffers, partly glibc not returning freed pages. See issue #57 for
  the breakdown and proposed fixes (drop the `SymbolManager` once
  names/files/lines are interned; resolve+drop per library to cap peak).

## Concurrency

- Multiple tool calls run in parallel against different profiles, or
  against the same `Arc<ProfileSession>` (queries are read-only over an
  immutable parsed profile). No global locks in the hot path.
- `load_profile` for two different paths runs concurrently. Loading the
  same path twice deduplicates: the second call awaits the first.

## Project setup

The repository structure, lints, and CI follow the patterns established in
[TimelyDataflow/differential-dataflow](https://github.com/TimelyDataflow/differential-dataflow),
keeping only what's needed for a single-binary crate.

### Toolchain

- **Edition: 2024.** Requires Rust 1.85+.
- **MSRV: 1.85** (the first stable release of edition 2024). Pinned in
  `Cargo.toml` under `[package].rust-version` and used by CI to gate
  builds.

### `Cargo.toml` shape

Single crate (no workspace yet — collapse to a workspace if/when a second
crate appears).

- `[package]` carries `edition = "2024"`, `rust-version = "1.85"`,
  `license = "MIT OR Apache-2.0"`, `repository`, `description`.
- `[lints.clippy]` adopts the curated set from differential-dataflow's
  `[workspace.lints.clippy]`. The list errs on the side of "warn on
  things that almost always indicate a bug or a stylistic regression"
  while explicitly allowing a small set that's noisy in practice
  (`type_complexity`, `option_map_unit_fn`, `wrong_self_convention`,
  `should_implement_trait`, `module_inception`).
- `[profile.release]` mirrors differential-dataflow's: `opt-level = 3`,
  `debug = true`, `lto = true`, `codegen-units = 4`. Useful for
  end-to-end performance testing without recompiling everything.

### CI (`.github/workflows/`)

Two workflows, both adapted from differential-dataflow:

**`test.yml`** — runs on push to `main` and on every PR. Four jobs:

1. **MSRV detection** — reads `rust-version` from `Cargo.toml`, exposes
   it as a job output.
2. **`cargo test`** — matrix over `{ubuntu, macos}` × `{stable, MSRV}`.
   Runs `cargo test --workspace --all-targets` and `cargo test --doc`.
   Windows is skipped initially (matches differential-dataflow's stance;
   revisit if there's demand). The end-to-end source/asm tests are part
   of the default test suite.
3. **`cargo fmt`** — `cargo fmt --all -- --check`. New job not in
   differential-dataflow.
4. **`cargo clippy`** — `cargo clippy --workspace --all-targets -- -D warnings`.
   Differential-dataflow does *not* fail on clippy warnings; pollard
   does.

**`release-plz.yml`** — runs `release-plz` on push to `main` to manage
version bumps and changelog entries. **Publishing is disabled by
default**: the workflow file is checked in, but `release-plz.toml`
contains `[workspace] release = false` (or per-package `publish = false`)
so no version is ever pushed to crates.io until that flag is flipped
intentionally. This keeps the release machinery wired up without risk of
accidental publication on early commits.

We also keep:
- **`dependabot.yml`** — weekly checks for GitHub Actions updates only
  (Cargo deps managed manually).

We don't carry over `deploy.yml` (mdbook deployment) or
`test-timely-master.yml` (testing against an upstream-master git
revision); neither applies here.

### `rustfmt.toml`

Default rustfmt is fine; no override file initially.

## Testing strategy

### What's tested in this crate

1. **Aggregation correctness** — `top_functions`, `call_tree`,
   `stacks_containing`, source/asm sample attribution.
2. **Pruning behavior** — `min_pct`, `max_depth`, `max_breadth`,
   linear-chain compression all behave deterministically; `_omitted` and
   `_truncated` markers appear correctly.
3. **Error paths** — `function_not_found` returns nearest matches,
   `function_ambiguous` lists candidates, `thread_not_found` /
   `process_not_found` list available alternatives, `invalid_value`
   names accepted enum variants, the unknown-event vs. stackless-marker
   split surfaces distinct messages, out-of-bounds `time_range` simply
   returns zero samples, and `profile_evicted` carries the original
   path.
4. **Output stability** — tool outputs are byte-stable across runs
   (snapshot tests catch unintended shape changes).
5. **MCP wire layer** — tool registration, JSON-schema validation, error
   envelope shape.

### Not tested here

- Symbolication correctness — `wholesym`'s problem.
- Profile parsing — `fxprof-processed-profile`'s problem.
- MCP framing — `rmcp`'s problem.

### Test types

**Unit tests on aggregation.** Build small synthetic profiles
programmatically using `fxprof-processed-profile`'s builder API. A test
sets up "thread T has these 50 samples with these stacks" and asserts
`top_functions(T)` returns the expected order. Fast, deterministic, no
real symbols required. This is the bulk of testing.

**Snapshot tests.** Run every tool against a checked-in small real
profile and snapshot the JSON output via `insta`. Catches accidental
output-shape drift; doubles as documentation of "what the LLM actually
sees."

**MCP integration tests.** Spawn the binary as a subprocess, send tool
calls over stdio JSON-RPC, assert responses. One happy path per tool plus
a couple of error envelopes.

**Source/asm end-to-end.** Build a tiny test binary in CI under
`tests/fixtures/`, record a profile against it as a build step, assert
that `source_for_function` returns expected lines and `asm_for_function`
returns expected instructions.

### Fixtures

- **Synthetic profiles** for unit tests — built in-process via
  `fxprof-processed-profile`'s builder, no files needed.
- **Tiny real profiles** for snapshot tests — checked in if they stay
  small (a few KB to tens of KB). The repository policy on larger
  fixtures is deferred until we hit a case that needs them; revisit then.

### Determinism plumbing

Aggregation outputs must be byte-stable for snapshot tests. The
tie-breaking rules in *Common conventions* above are part of the spec for
exactly this reason — without them, hash-map iteration order leaks into
output.

### Out of scope for v1 testing

- LLM-quality evals (does the output actually help Claude solve real
  problems). Worth doing eventually but needs an eval harness.
- Performance / scale tests on large profiles. Add when we hit a real
  case.
- Property-based tests (e.g. `proptest` on synthetic profiles).
  Nice-to-have.
