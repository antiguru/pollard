# Transcripts

Side-by-side recordings of the same investigation in two settings:

* `*-without-pollard.md` — the assistant has shell access (can run `samply`, read source, write ad-hoc python against the JSON via [`top_funcs.py`](./top_funcs.py)) but no profile-query tools.
* `*-with-pollard.md` — the assistant can call pollard's MCP tools (`load_profile`, `top_functions`, `source_for_function`, `compare_profiles`).

Each session is opened with the same user prompt:

> I have a program X and noticed it's slow. Please profile and find the root cause.

The `with-pollard` transcripts use real tool output captured while preparing the demo; the figures (sample counts, percentages, deltas) are reproducible with the binaries in this crate.

| Binary | Without pollard | With pollard |
|---|---|---|
| `log_p99` | [log_p99-without-pollard.md](./log_p99-without-pollard.md) | [log_p99-with-pollard.md](./log_p99-with-pollard.md) |
| `matmul` | [matmul-without-pollard.md](./matmul-without-pollard.md) | [matmul-with-pollard.md](./matmul-with-pollard.md) |
| `nested_join` | [nested_join-without-pollard.md](./nested_join-without-pollard.md) | [nested_join-with-pollard.md](./nested_join-with-pollard.md) |
| `page_fault` | [page_fault-without-pollard.md](./page_fault-without-pollard.md) | [page_fault-with-pollard.md](./page_fault-with-pollard.md) |

## Reproducing the with-pollard side

```bash
cargo build --profile demo -p pollard-demo

# cycles profiles (samply's own recorder)
for bin in log_p99 matmul nested_join; do
  for mode in slow fast; do
    samply record --save-only \
      -o profiles/${bin}-${mode}.json.gz \
      "$PWD/target/demo/${bin}" "$mode"
  done
done

# page-faults profile (Linux only; perf records, samply converts)
for mode in slow fast; do
  perf record -e page-faults --call-graph dwarf \
    -o profiles/page_fault-${mode}.perf.data \
    "$PWD/target/demo/page_fault" "$mode"
  samply import profiles/page_fault-${mode}.perf.data \
    -o profiles/page_fault-${mode}.json.gz --save-only
done
```

Then in a Claude Code session with the pollard plugin installed, call `load_profile` on the profile of interest and follow the tool sequence shown in the transcript.
