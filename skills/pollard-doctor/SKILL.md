---
name: pollard-doctor
description: Diagnose a broken pollard install — MCP server unreachable, missing binaries, profile load failures, unsymbolicated frames. Use when any pollard tool call fails, when load_profile errors out, or when the user reports "the profiler isn't working."
---

# Pollard doctor

Walk through the install in dependency order, stop at the first
failing check, and surface the exact remediation. The plugin install
only writes MCP config — it does not put binaries on `PATH` or seed
any state — so most "doesn't work" reports trace back to one of three
things: missing binary, MCP server config not reloaded, or a profile
that's malformed for reasons external to pollard.

## Steps

### 1. Binaries on PATH

```sh
pollard --version
samply  --version
```

- `pollard: command not found` → `cargo install pollard`
- `pollard --version` prints but is older than expected →
  `cargo install pollard --force` to overwrite.
- `samply: command not found` → `cargo install --locked samply`

If `cargo` itself is missing, point the user at <https://rustup.rs/>
before retrying.

### 2. MCP server reachable

The agent already has access to pollard tools if this skill was
invoked successfully — but if the user just installed the plugin and
the tool list is stale, they need to reload. Confirm by attempting
the lightest-weight tool call:

```text
call list_profiles
```

- Returns an empty list or any list at all → server is up.
- "Tool not found" or similar → the MCP client hasn't picked up the
  plugin's `mcpServers` registration yet. In Claude Code, run
  `/plugin reload` or restart the session. Other clients have their
  own reload story; check their docs.
- Server starts but every call hangs → likely a stale binary on
  `PATH`. Re-run `cargo install pollard --force` to overwrite.

### 3. A profile actually loads

Synthetic test:

```sh
samply record --save-only -o /tmp/pollard-doctor.json.gz -- /bin/ls /
```

Then:

```text
call load_profile with path="/tmp/pollard-doctor.json.gz"
```

- Returns a `profile_id` and a non-zero `description.duration_ms` →
  end-to-end works. Any failure beyond this point is profile-specific,
  not install-specific.
- Returns an empty profile (no samples): `/bin/ls` finished before any
  sample landed. Re-run with `-r 4000` for a higher rate, or pick a
  longer-running command.
- `load_profile` errors with a parse failure: the file isn't
  Firefox-format. Confirm with `file /tmp/pollard-doctor.json.gz` —
  should report gzip; if it's plain JSON, that's also fine.

### 4. Symbols resolve

After a successful `load_profile`, check `describe_profile` (or the
description returned by `load_profile`):

- `unsymbolicated_pct` near 0 → symbols are healthy.
- `unsymbolicated_pct` high → the binary was stripped or built
  without debug info, or the matching debug info isn't on the local
  filesystem.
  - Rust: rebuild with `RUSTFLAGS=-Cdebuginfo=2`, or set `[profile.release] debug = true` in `Cargo.toml`.
  - C/C++: rebuild with `-g`. For stripped system binaries, install
    the corresponding `*-dbg` / `*-debuginfo` package.
  - Cross-host imports (`perf.data` from a different machine):
    pollard symbolicates against the local filesystem, so symbols
    won't resolve unless both binaries and matching debug info are
    present locally. Re-record on the host you're analyzing on.

### 5. Profile-specific issues

If the install passes 1–4 but the user's actual profile still
behaves oddly:

- Stacks dominated by `Layered<…>`, `tokio::runtime`, or stdlib
  glue → not a bug, just noise. Apply the `view-presets` skill to
  build a `create_view` that filters them.
- `top_functions` returns `<unknown>` or `[unsymbolicated]` heavy →
  see step 4.
- Tools time out on a huge profile → check `describe_profile` for
  sample counts; a 2 GB profile is genuinely heavy. Use
  `time_range` or `process` filters via `create_view` to narrow the
  slice before running expensive aggregations.

## Reporting

After the diagnostic, summarize for the user:

- Which checks passed, which failed.
- The exact remediation command for the first failing step.
- If everything passed but their original symptom persists, escalate
  to issue-filing — capture `pollard --version`, OS, the failing
  command, and any error output.
