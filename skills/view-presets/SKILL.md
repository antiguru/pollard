---
name: view-presets
description: Apply canonical hide_modules / hide_frames regex sets to a loaded profile via pollard's create_view. Use when a Rust profile is dominated by framework noise — tracing-subscriber Layered walls, tokio runtime internals, or Rust stdlib glue (drop, format, panic) — and you want to focus on user code.
---

# View presets for pollard

A copy-paste reference of `hide_modules` / `hide_frames` patterns that
keep showing up when reading Rust profiles. Use them as building
blocks for `create_view`: paste the relevant blocks, drop rules that
don't help on the current profile, then verify with `rule_stats`
(`frames_matched: 0` means a pattern compiled but never matched
anything — fix or drop it).

These are starting points, not an exhaustive curated list. Real
profiles have crates these don't cover and frames these over-match;
converging on the right set per codebase is part of the workflow.

## Steps

1. Run `summary` (or `top_functions`) on the loaded profile to
   confirm the noise category before applying a preset:
   - tracing-subscriber walls: lots of `Layered<...>` frames, mentions
     of `tracing_core`, `tracing_subscriber::fmt::*`.
   - tokio internals: `tokio::runtime::*`, `tokio::task::*`, `mio::*`
     dominate without your code in between.
   - stdlib glue: top frames are `core::ptr::drop_in_place`,
     `core::fmt::*`, `alloc::*`, panic / unwind paths.
2. Pick the preset block(s) below that match. Multiple blocks
   compose — paste both module lists and both frame lists into a
   single `create_view`.
3. Call `create_view` with the chosen patterns and a descriptive
   `name` (e.g. `"no-tracing-noise"`).
4. Read the returned `rule_stats`. Drop any rule with
   `frames_matched: 0`. If `samples_affected` is close to
   `total_base_samples` for a single rule, the rule is too broad —
   narrow it.
5. For project-specific filters, stack a second view on top of the
   noise view (pass the noise view's id as the new `profile_id`)
   instead of merging into one big call. Stacking keeps each layer's
   `rule_stats` independently reviewable.

## Pattern syntax

- Substring match by default: `tokio` matches any module / frame name
  containing the literal string `tokio`.
- Regex match with `re:` prefix: `re:^tokio::` is anchored to the
  start.
- Inline regex flags work: `re:(?i)memcpy` is case-insensitive.
- HTML-encoded `&lt;` / `&gt;` are decoded automatically — useful if
  the LLM generated them in generic types.

## Preset: tracing-subscriber noise

```jsonc
{
  "hide_modules": [
    "re:^tracing(_subscriber|_core)?$",
    "tracing_log",
    "tracing_attributes"
  ],
  "hide_frames": [
    "re:^tracing_subscriber::",
    "re:^tracing_core::",
    "re:<tracing_subscriber::.*Layered.*>::",
    "re:^tracing::span::",
    "re:^tracing::__macro_support::"
  ]
}
```

Pair with `strip_type_params: true` to fold residual `Layered<…>`
monomorphisations together.

## Preset: tokio runtime internals

```jsonc
{
  "hide_modules": [
    "re:^tokio(_util|_stream|_io|_macros)?$",
    "mio",
    "parking_lot",
    "parking_lot_core",
    "crossbeam_utils",
    "crossbeam_epoch"
  ],
  "hide_frames": [
    "re:^tokio::runtime::",
    "re:^tokio::task::",
    "re:^tokio::time::",
    "re:^tokio::sync::",
    "re:^<tokio::.*Runtime.*>::",
    "re:^mio::",
    "re:^<.*futures_util::.*>::poll",
    "re:^<.*futures_core::.*>::poll"
  ]
}
```

Set `collapse_recursion: true` alongside this preset when the
executor re-enters itself — `block_on → poll → block_on → poll …`
collapses to one occurrence.

## Preset: Rust stdlib glue

```jsonc
{
  "hide_frames": [
    "re:^core::ptr::drop_in_place",
    "re:^<.* as core::ops::drop::Drop>::drop",
    "re:^alloc::",
    "re:^core::fmt::",
    "re:^<core::fmt::",
    "re:^std::fmt::",
    "re:^core::panicking::",
    "re:^std::panicking::",
    "re:^rust_begin_unwind",
    "re:^_Unwind_",
    "re:^core::result::",
    "re:^core::option::",
    "re:^core::iter::",
    "re:^core::slice::iter::"
  ],
  "hide_modules": [
    "re:^std$",
    "re:^core$",
    "re:^alloc$"
  ]
}
```

The `hide_modules` half is aggressive — it drops *everything* from
the standard crates. Use it on profiles where you only care about
your own crate; drop it when `Vec::push` or `HashMap::insert` is
genuinely the hot frame you're chasing.

## Combining presets via stacked views

```jsonc
// view 1: noise filter
{
  "profile_id": "<base>",
  "name": "no-noise",
  "hide_modules": ["re:^tracing(_subscriber|_core)?$", "re:^tokio.*$", "mio", "parking_lot"],
  "hide_frames": ["re:^core::ptr::drop_in_place", "re:^core::fmt::", "re:^alloc::"]
}

// view 2: stacked on the noise view, project-specific
{
  "profile_id": "<no-noise view id>",
  "name": "focus-query",
  "keep_only_modules": ["my_crate", "my_crate_query"]
}
```

Stacking is cheap — transforms compose at aggregation time — and
keeps each layer's `rule_stats` independently reviewable.

## Reading rule_stats

After every `create_view`, inspect the per-rule diagnostics:

- `frames_matched: 0` → pattern is wrong or doesn't apply to this
  profile. Drop the rule or fix the regex.
- `samples_affected` close to `total_base_samples` → the rule is
  hiding most of the profile. Probably too broad; narrow it.
- A single `hide_*` rule that explains 80%+ of the noise → consider
  promoting it to the project's default.
