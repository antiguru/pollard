# log_p99 — without pollard

## User

> I have a program `log_p99` and noticed it's slow. Please profile and find the root cause.

## Assistant

Without profile-reading tools I can record a profile and inspect it indirectly, but I can't drill into it the way a profile viewer can.

Let me record:

```bash
$ samply record --save-only -o /tmp/log_p99-slow.json.gz ./target/demo/log_p99 slow
mode=slow  records=10000000  elapsed=3.197s  checksum=0x00000000c5a18a62
```

I have a 31 KB gzipped Firefox-format profile at `/tmp/log_p99-slow.json.gz`. To find the hot function I can:

1. Decompress and look at the `funcTable.name` and `samples` arrays directly:

```bash
$ gunzip -c /tmp/log_p99-slow.json.gz | jq '.threads[0].samples.stack | length'
3212
$ gunzip -c /tmp/log_p99-slow.json.gz | jq '.threads[0].funcTable.name | length'
1147
```

That's 3212 samples across 1147 distinct functions. The stack table indirection (`samples.stack[i] -> stackTable -> frameTable -> funcTable.name`) is layered enough that flattening it correctly in `jq` is fragile — I can write the join, but the resulting one-liner is hard to verify and easy to get wrong on an edge case (mixed inline frames, native vs. label frames).

2. Or open the profile in the Firefox Profiler UI:

```bash
$ samply load /tmp/log_p99-slow.json.gz
# server starts on http://localhost:NNNN, browser tab opens
```

This is what I'd ask you to do. Could you open the profile and report back:

* The top 3 entries in **Call Tree → Invert**, by self time?
* For each one, the caller two or three frames up?
* If `format!`, `__rdl_alloc`, or `sort` show up in the top, please say so.

Once I know which function dominates, I can read the source for that function and propose a fix. Right now I'm flying half-blind: I can compile, I can run, I can record — but the analysis step is bottlenecked on you driving the UI.

## What this workflow misses

* No way to programmatically pull "top 15 functions by self samples" — that's a UI click for the human.
* No way to ask "what fraction of time is in this specific line of `run_slow`" without screenshots.
* Iteration is slow: every "what about this other function?" is another round-trip through you.
* I never see the per-line attribution that a `source_for_function`-style tool gives, so the root cause stays vague ("sorting is slow") instead of specific ("line 54, sort_unstable, 81% self").
