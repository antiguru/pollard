# pollard

A Model Context Protocol (MCP) server that exposes Firefox-format performance
profiles (such as those produced by [`samply`](https://github.com/mstange/samply))
to AI coding assistants.

The name comes from *pollarding* — the forestry technique of pruning a tree's
upper branches to encourage dense, manageable regrowth — which is exactly what
this tool does to call trees so an LLM can hold them.

## Status

Early development. Not yet published to crates.io.

## Tools

`pollard` exposes 14 MCP tools:

* **Lifecycle:** `load_profile`, `unload_profile`, `list_profiles`,
  `describe_profile`, `summary`
* **Query:** `top_functions`, `top_groups`, `call_tree`, `stacks_containing`,
  `folded_stacks`, `compare_profiles`
* **Drill-down:** `source_for_function`, `asm_for_function`,
  `address_to_function`

See `docs/superpowers/specs/2026-04-28-pollard-design.md` for full details.

## Install

Once published to crates.io:

```sh
cargo install pollard
```

Until then, install from this repository:

```sh
cargo install --git https://github.com/antiguru/pollard
```

Either form puts a `pollard` binary on your `PATH`.
Register it with Claude Code as a user-scoped MCP server:

```sh
claude mcp add pollard pollard --scope user
```

After that, `load_profile`, `top_functions`, `call_tree` and the rest of the tools listed above are available in any Claude Code session.

## Build from source

```sh
cargo build --release
```

## License

MIT OR Apache-2.0.
