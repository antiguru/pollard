---
name: profile-recording
description: Record a performance profile with samply (or convert an existing perf.data via samply import) and load it into pollard for analysis. Use when the user wants to profile a binary, a one-shot command, an already-running process, or analyze an existing perf.data file.
---

# Recording a profile for pollard

`pollard` reads Firefox-format profile JSON. The two paths to that
format are `samply record` (record a fresh profile) and `samply import`
(convert an existing `perf.data` from `perf record`). Pick the path
that matches what the user has in hand, run the command, then call
`load_profile`.

## When to use which path

| User has | Use |
|----------|-----|
| A command they want to profile | `samply record <cmd>` |
| A long-running process / pid | `samply record -p <pid>` |
| An existing `perf.data` from `perf record` | `samply import perf.data` |
| A `.json` / `.json.gz` already produced by samply | skip recording, go straight to `load_profile` |

## Steps

### Path A — record a command

1. Confirm `samply` is on `PATH`:
   ```sh
   which samply || cargo install --locked samply
   ```
2. Record with `--save-only` so samply writes a file instead of opening
   the web UI. Pick an output path the user can keep around:
   ```sh
   samply record --save-only -o /tmp/profile.json.gz -- <cmd> [args...]
   ```
   - `--` separates samply's args from the command's args.
   - Default sampling rate is 1000 Hz; pass `-r 4000` to crank it up
     for short-running commands.
   - On Linux, samply uses the kernel's `perf_event_open`. If the
     command exits before any samples land (very fast commands), wrap
     it in a loop or use a higher rate.
3. Pass the saved file to pollard:
   ```text
   call load_profile with path="/tmp/profile.json.gz"
   ```

### Path B — attach to a running process

1. Find the pid (`pidof <name>`, `pgrep -f <pattern>`).
2. Record for a fixed duration:
   ```sh
   samply record --save-only -o /tmp/profile.json.gz -p <pid> --duration 30
   ```
   Without `--duration`, samply records until you Ctrl-C; in an
   automated session prefer the explicit duration.
3. Call `load_profile` with the saved file.

### Path C — convert an existing `perf.data`

The user already has `perf.data` from `perf record -g` or similar.
samply converts it to Firefox-format:

1. ```sh
   samply import perf.data --save-only -o /tmp/profile.json.gz
   ```
   - Pass `--unstable-presymbolicate` only if the user explicitly
     wants symbols baked in at import time; otherwise pollard
     symbolicates on load and benefits from caching.
   - `samply import` accepts perf script text via stdin too; rare,
     but useful if only the script form is available.
2. Call `load_profile` with the saved file.

### Path D — already a Firefox-format profile

Files ending in `.json`, `.json.gz`, or `.zip` produced by samply or
the Firefox Profiler can be loaded directly:

```text
call load_profile with path="<path>"
```

No conversion step needed.

## After loading

Once `load_profile` returns, the agent has the full pollard tool
surface available — `top_functions`, `call_tree`, `summary`,
`create_view`, etc. Two quick follow-ups that almost always pay off:

1. **`summary`** for a one-shot orientation (top frames, problematic
   modules, hints toward `create_view` if the profile is noisy).
2. **`create_view`** with appropriate `hide_modules` / `hide_frames`
   patterns when the raw profile is dominated by framework noise.
   The `view-presets` skill (also in this plugin) lists copy-paste
   regex sets for tracing-subscriber, tokio, and stdlib glue.

## Common pitfalls

- **Empty profile** (`samples: 0`): the command finished before any
  sample landed. Re-record with `-r 4000` or a longer-running command.
- **Unsymbolicated frames** (`unsymbolicated_pct` high in
  `describe_profile`): the binary was stripped or built without debug
  info. Rebuild with `RUSTFLAGS=-Cdebuginfo=2` (Rust) or `-g` (C/C++)
  and re-record.
- **`perf.data` from a different machine**: symbols resolve against
  the local filesystem, so cross-host imports lose names unless the
  binaries and matching debug info are present locally.
- **`perf.data` recorded without `-g`**: no call stacks, only flat
  function counts. Re-record with `-g` (frame pointers) or
  `--call-graph dwarf` (DWARF unwinding, larger files but works
  without frame pointers).
