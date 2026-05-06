# pollard

A Model Context Protocol (MCP) server that exposes Firefox-format performance
profiles (such as those produced by [`samply`](https://github.com/mstange/samply))
to AI coding assistants.

The name comes from *pollarding* — the forestry technique of pruning a tree's
upper branches to encourage dense, manageable regrowth — which is exactly what
this tool does to call trees so an LLM can hold them.

## Status

Early development. Published to [crates.io](https://crates.io/crates/pollard).

## Tools

`pollard` exposes 16 MCP tools:

* **Lifecycle:** `load_profile`, `unload_profile`, `list_profiles`,
  `create_view`, `describe_profile`, `summary`
* **Query:** `top_functions`, `top_groups`, `call_tree`, `stacks_containing`,
  `folded_stacks`, `compare_profiles`
* **Drill-down:** `source_for_function`, `asm_for_function`,
  `address_to_function`, `compare_functions`

`top_functions`, `call_tree`, and `compare_profiles` accept an optional
`event` argument: omit it to aggregate the default samples track
(cycles, in samply's perf recorder) or pass a marker name like
`cache-misses`, `branch-misses`, or `instructions` to aggregate that
hardware counter instead.
`top_groups` currently aggregates samples only.

See `docs/superpowers/specs/2026-04-28-pollard-design.md` for full details.
See `docs/superpowers/specs/2026-05-06-view-presets-cookbook.md` for
copy-paste `hide_modules` / `hide_frames` regex sets covering common
Rust noise (tracing-subscriber, tokio internals, stdlib glue). The same
cookbook ships as the `pollard:view-presets` skill — see
`.claude-plugin/` for the bundled plugin layout that registers both the
MCP server and the skills.

## Install

The `pollard` binary always has to be on your `PATH` — the plugin
bundle does not ship it. Install with cargo:

```sh
cargo install pollard
```

Or build the latest from this repository:

```sh
cargo install --git https://github.com/antiguru/pollard
```

Then pick one of two ways to register pollard with Claude Code.

### Option 1 — Claude Code plugin (recommended)

Installs the MCP server *and* the bundled skills (`profile-recording`,
`view-presets`, `pollard-doctor`) in one step:

```text
/plugin marketplace add antiguru/pollard
/plugin install pollard@antiguru
```

Skills surface as `/pollard:<skill>` alongside everything else the
user has installed. If a tool call fails after install, run
`/pollard:pollard-doctor` — the doctor walks the install in dependency
order and surfaces the exact remediation.

### Option 2 — MCP server only

If you only want the tools and not the skills, register the binary
directly as a user-scoped MCP server:

```sh
claude mcp add pollard pollard --scope user
```

Either path makes `load_profile`, `top_functions`, `call_tree` and
the rest of the tools listed above available in any Claude Code
session.

## Build from source

```sh
cargo build --release
```

## License

MIT OR Apache-2.0.
