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

`pollard` exposes 9 MCP tools:

- **Lifecycle:** `load_profile`, `unload_profile`, `list_profiles`,
  `describe_profile`
- **Query:** `top_functions`, `call_tree`, `stacks_containing`
- **Drill-down:** `source_for_function`, `asm_for_function`

See `docs/superpowers/specs/2026-04-28-pollard-design.md` for full details.

## Build

```sh
cargo build --release
```

## License

MIT OR Apache-2.0.
