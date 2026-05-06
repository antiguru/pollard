# view presets — cookbook

A copy-paste reference of `hide_modules` / `hide_frames` regex sets that
keep showing up when reading Rust profiles with `create_view`. These are
*examples*, not built-in presets — paste what you need into
`create_view`, drop the rules that don't help on your profile, and check
`rule_stats` afterwards (`frames_matched: 0` means the pattern compiled
but never matched anything in this profile, which is the typo signal).

Why a cookbook instead of a `presets=[...]` argument: preset content
drifts as upstream crates rename internals, and pinning a curated list
in code biases users toward whatever was popular when it was written. A
doc page is easy to update and easy to fork. We may revisit a built-in
preset arg once `hide_modules` patterns from real usage have stabilised
— see issue #92.

All patterns below use `pollard`'s matcher syntax: substring by default,
regex when prefixed with `re:`. Module patterns match against the lib /
crate name samply attaches to each frame; frame patterns match the
demangled function name. Use `(?i)` inside a regex for
case-insensitive matching.

## tracing-subscriber noise

`tracing-subscriber`'s `Layered<…, Layered<…, Layered<…, Registry>>>`
stack puts the same handful of frames between every span boundary and
the user code. Hiding the modules collapses those walls without
discarding any user frames.

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

Pair with `strip_type_params: true` if you want the residual
`Layered<…>::on_event` frames to fold together — otherwise each
distinct subscriber stack symbolicates as its own monomorphisation.

## tokio runtime internals

The async runtime's poll loop, scheduler, park / unpark, and timer
wheel typically dominate a sample-by-time profile of an idle service
without telling you anything about your own code. Hide them when you
want a CPU-on-user-work view; keep them when you're investigating
runtime behaviour itself.

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

Set `collapse_recursion: true` alongside this preset when the executor
re-enters itself: `Runtime::block_on → poll → block_on → poll …` then
shows up once instead of N times.

## Rust stdlib glue

Drop glue, panic landing pads, and formatting machinery clutter almost
every profile. None of these frames are usually the culprit for the
sample they sit on; the next frame down is.

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

The `hide_modules` half is aggressive — it drops *everything* that
symbolicates to one of the standard crates, not just glue. Use it on
profiles where you only care about your own crate; drop it when
`Vec::push` or `HashMap::insert` is genuinely the hot frame you're
chasing.

## combining presets

`create_view` accepts the union — paste both module lists and both
frame lists into a single call. Then stack a second view on top with
your project-specific rules so you can tweak them without re-running
`rule_stats` for the noise filters.

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

Stacking is cheap (transforms compose at aggregation time) and keeps
each layer's `rule_stats` independently reviewable.

## reading rule_stats

After every `create_view`, look at the per-rule diagnostics:

- `frames_matched: 0` → pattern is wrong or doesn't apply to this
  profile. Drop the rule or fix the regex.
- `samples_affected` close to `total_base_samples` → the rule is
  hiding most of the profile. Probably too broad; narrow it.
- A single `hide_*` rule that explains 80%+ of the noise → consider
  promoting it to the project's default cookbook entry.

The patterns above are starting points, not gospel. Real profiles will
have crates these rules don't cover and frames these rules over-match;
the `rule_stats` loop is how you converge on the right set for your
codebase.
