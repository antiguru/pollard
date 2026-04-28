# pollard MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `pollard`, a stdio MCP server in Rust that loads Firefox-format performance profiles and exposes 9 query tools (load/unload/list/describe + top_functions, call_tree, stacks_containing, source_for_function, asm_for_function) so AI coding assistants can analyze profiles without re-parsing per query.

**Architecture:** Single Rust binary (Rust 2024). Two layers: a query module that aggregates over a parsed Firefox profile (pure, deterministic), and an MCP layer (`rmcp` 1.5) that registers tools, parses arguments, and serializes responses. A partial deserialization layer mirrors the documented Firefox processed-profile schema for just the fields we touch (samples, stacks, frames, funcs, libs, threads, processes, strings). Symbolication uses `wholesym`. Source/asm endpoints reuse `samply-api`. Memory is bounded by an LRU registry (default 4 profiles).

**Tech Stack:** Rust 2024 (MSRV 1.85), `rmcp 1.5`, `tokio`, `wholesym 0.8`, `samply-api 0.24`, `samply-symbols`, `serde`, `serde_json`, `flate2`, `regex`, `insta` (dev). `release-plz` for releases (publishing disabled by default). CI on GitHub Actions: `cargo fmt --check`, `cargo test`, `cargo clippy -- -D warnings`.

**Spec:** `docs/superpowers/specs/2026-04-28-pollard-design.md`. Refer to it for data shapes, error envelopes, and pruning policy details.

---

## File Structure

```
pollard/
├── Cargo.toml
├── LICENSE-MIT
├── LICENSE-APACHE
├── README.md
├── .gitignore
├── release-plz.toml
├── .github/
│   ├── dependabot.yml
│   └── workflows/
│       ├── test.yml
│       └── release-plz.yml
├── src/
│   ├── main.rs                       # binary entry; wires rmcp server, tokio runtime
│   ├── error.rs                      # ToolError type, structured payloads
│   ├── matching.rs                   # function matching: substring + re: prefix
│   ├── profile/                      # PARTIAL deserialization of Firefox profile JSON
│   │   ├── mod.rs                    # public types and re-exports
│   │   ├── raw.rs                    # raw deserialization (serde-derived structs)
│   │   ├── parsed.rs                 # ergonomic accessors over raw types
│   │   └── load.rs                   # file → Profile (handles .json / .json.gz)
│   ├── session.rs                    # ProfileSession: parsed + symbolicated + path/id
│   ├── registry.rs                   # SessionRegistry with LRU eviction
│   ├── query/
│   │   ├── mod.rs                    # shared output types: Frame, FrameRef, Pct
│   │   ├── filters.rs                # thread/process/time_range filtering
│   │   ├── top_functions.rs
│   │   ├── call_tree.rs              # tree build + pruning (min_pct, max_depth, max_breadth, chain)
│   │   ├── stacks_containing.rs
│   │   ├── source.rs                 # source_for_function (uses samply-api /source/v1)
│   │   └── asm.rs                    # asm_for_function (uses samply-api /asm/v1)
│   └── tools/
│       ├── mod.rs                    # tool list/registration; common arg parsing
│       ├── lifecycle.rs              # load_profile, unload_profile, list_profiles, describe_profile
│       ├── query.rs                  # top_functions, call_tree, stacks_containing
│       └── drill_down.rs             # source_for_function, asm_for_function
└── tests/
    ├── helpers/
    │   └── synthetic.rs              # fxprof-processed-profile builders for test profiles
    ├── snapshot.rs                   # insta snapshots of every tool against a tiny real profile
    ├── snapshots/                    # insta-managed
    ├── mcp_integration.rs            # spawn binary, JSON-RPC over stdio, assert
    ├── e2e_source_asm.rs             # build tiny C binary, record, assert source/asm
    └── fixtures/
        ├── tiny.json.gz              # checked-in small real profile (snapshot tests)
        └── tiny_program.c            # source for e2e binary
```

**Why this decomposition:**
- `profile/` is its own module because deserialization is a substantial subsystem with its own correctness surface.
- `query/` and `tools/` are split because `query/` is pure aggregation logic (testable without rmcp), and `tools/` is the rmcp adapter (thin, easy to test via spawn).
- `tools/` is split into 3 files (lifecycle / query / drill_down) by concern; each file is ~150-200 lines.

---

## Phase 0 — Repository skeleton

### Task 1: License files

**Files:**
- Create: `LICENSE-MIT`
- Create: `LICENSE-APACHE`

- [ ] **Step 1: Add `LICENSE-MIT`** with the standard MIT license text.

```
MIT License

Copyright (c) 2026 Moritz Hoffmann

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

- [ ] **Step 2: Add `LICENSE-APACHE`** with the standard Apache-2.0 license text. Copy from <https://www.apache.org/licenses/LICENSE-2.0.txt>.

### Task 2: `.gitignore`

**Files:**
- Create: `.gitignore`

- [ ] **Step 1: Add the file**

```
/target
**/*.rs.bk
Cargo.lock
.vscode/
.idea/
*.swp
.DS_Store
```

### Task 3: `Cargo.toml`

**Files:**
- Create: `Cargo.toml`

- [ ] **Step 1: Write the manifest**

```toml
[package]
name = "pollard"
version = "0.0.1"
edition = "2024"
rust-version = "1.85"
license = "MIT OR Apache-2.0"
description = "MCP server that exposes Firefox-format performance profiles to AI coding assistants."
repository = "https://github.com/<user>/pollard"
keywords = ["profiling", "mcp", "ai", "firefox-profiler", "samply"]
categories = ["development-tools::profiling", "command-line-utilities"]

[dependencies]
rmcp = { version = "1.5", features = ["server", "transport-io", "macros"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "io-std", "sync", "process"] }
wholesym = "0.8"
samply-api = "0.24"
samply-symbols = "0.27"
fxprof-processed-profile = "0.8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
flate2 = "1"
regex = "1"
debugid = "0.8"

[dev-dependencies]
insta = { version = "1", features = ["json", "yaml"] }
tempfile = "3"

[lints.clippy]
type_complexity = "allow"
option_map_unit_fn = "allow"
wrong_self_convention = "allow"
should_implement_trait = "allow"
module_inception = "allow"

bool_comparison = "warn"
borrow_interior_mutable_const = "warn"
borrowed_box = "warn"
builtin_type_shadow = "warn"
clone_on_ref_ptr = "warn"
crosspointer_transmute = "warn"
dbg_macro = "warn"
deref_addrof = "warn"
disallowed_macros = "warn"
disallowed_methods = "warn"
disallowed_types = "warn"
double_must_use = "warn"
double_parens = "warn"
duplicate_underscore_argument = "warn"
excessive_precision = "warn"
extra_unused_lifetimes = "warn"
from_over_into = "warn"
match_overlapping_arm = "warn"
must_use_unit = "warn"
mut_mutex_lock = "warn"
needless_borrow = "warn"
needless_pass_by_ref_mut = "warn"
needless_question_mark = "warn"
needless_return = "warn"
no_effect = "warn"
panicking_overflow_checks = "warn"
partialeq_ne_impl = "warn"
print_literal = "warn"
redundant_closure = "warn"
redundant_closure_call = "warn"
redundant_field_names = "warn"
redundant_pattern = "warn"
redundant_slicing = "warn"
redundant_static_lifetimes = "warn"
same_item_push = "warn"
shadow_unrelated = "warn"
single_component_path_imports = "warn"
suspicious_assignment_formatting = "warn"
suspicious_else_formatting = "warn"
suspicious_unary_op_formatting = "warn"
todo = "warn"
transmutes_expressible_as_ptr_casts = "warn"
unnecessary_cast = "warn"
unnecessary_lazy_evaluations = "warn"
unnecessary_mut_passed = "warn"
unnecessary_unwrap = "warn"
unused_async = "warn"
useless_asref = "warn"
useless_conversion = "warn"
useless_format = "warn"
wildcard_in_or_patterns = "warn"
write_literal = "warn"
zero_divided_by_zero = "warn"
zero_prefixed_literal = "warn"

[profile.release]
opt-level = 3
debug = true
rpath = false
lto = true
debug-assertions = false
codegen-units = 4
```

- [ ] **Step 2: Add `src/main.rs` placeholder**

```rust
fn main() {
    println!("pollard placeholder");
}
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build`
Expected: compiles with no warnings; some deps may take a few minutes the first time.

- [ ] **Step 4: Verify clippy is clean**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings, no errors.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml LICENSE-MIT LICENSE-APACHE .gitignore src/main.rs
git commit -m "chore: bootstrap pollard crate"
```

(Cargo.lock is in `.gitignore` per the standard library-style convention; revisit if pollard is reclassified as a binary-only application later.)

### Task 4: GitHub workflows

**Files:**
- Create: `.github/dependabot.yml`
- Create: `.github/workflows/test.yml`
- Create: `.github/workflows/release-plz.yml`
- Create: `release-plz.toml`

- [ ] **Step 1: `.github/dependabot.yml`**

```yaml
version: 2
updates:
  - package-ecosystem: "github-actions"
    directory: "/"
    schedule:
      interval: "weekly"
```

- [ ] **Step 2: `.github/workflows/test.yml`**

```yaml
name: "Test Suite"
on:
  push:
    branches:
      - main
  pull_request:

jobs:
  msrv:
    name: Determine MSRV
    runs-on: ubuntu-latest
    outputs:
      msrv: ${{ steps.msrv.outputs.msrv }}
    steps:
      - uses: actions/checkout@v6
      - id: msrv
        run: echo "msrv=$(grep '^rust-version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')" >> "$GITHUB_OUTPUT"

  test:
    needs: msrv
    strategy:
      matrix:
        os:
          - ubuntu
          - macos
        toolchain:
          - stable
          - ${{ needs.msrv.outputs.msrv }}
    name: cargo test on ${{ matrix.os }}, rust ${{ matrix.toolchain }}
    runs-on: ${{ matrix.os }}-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          toolchain: ${{ matrix.toolchain }}
      - name: Cargo test
        run: cargo test --workspace --all-targets
      - name: Cargo doc test
        run: cargo test --doc

  fmt:
    name: Cargo fmt
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          components: rustfmt
      - name: Cargo fmt
        run: cargo fmt --all -- --check

  clippy:
    name: Cargo clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          components: clippy
      - name: Cargo clippy
        run: cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 3: `.github/workflows/release-plz.yml`**

```yaml
name: Release-plz

permissions:
  pull-requests: write
  contents: write

on:
  push:
    branches:
      - main

concurrency:
  group: release-plz

jobs:
  release-plz:
    name: Release-plz
    runs-on: ubuntu-latest
    steps:
      - name: Checkout repository
        uses: actions/checkout@v6
        with:
          fetch-depth: 0
      - name: Install Rust toolchain
        uses: actions-rust-lang/setup-rust-toolchain@v1
      - name: Run release-plz
        uses: MarcoIeni/release-plz-action@v0.5
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          CARGO_REGISTRY_TOKEN: ${{ secrets.POLLARD_RELEASE_PLZ }}
```

- [ ] **Step 4: `release-plz.toml` — disable publishing**

```toml
[workspace]
publish = false
release = false
```

`publish = false` blocks `cargo publish`. `release = false` causes release-plz to skip the version-bump PR as well. To enable releases later: flip both to `true` (or remove the entries).

- [ ] **Step 5: Commit**

```bash
git add .github release-plz.toml
git commit -m "ci: add test, release-plz, dependabot workflows"
```

### Task 5: README.md

**Files:**
- Create: `README.md`

- [ ] **Step 1: Write a short README**

```markdown
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
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: add README"
```

---

## Phase 1 — Foundations

### Task 6: `error.rs` — structured tool errors

**Files:**
- Create: `src/error.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_not_found_serializes_with_nearest_matches() {
        let err = ToolError::FunctionNotFound {
            function: "memcyp".into(),
            nearest_matches: vec!["memcpy".into(), "mempcpy".into()],
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error"], "function_not_found");
        assert_eq!(json["function"], "memcyp");
        assert_eq!(json["nearest_matches"][0], "memcpy");
    }

    #[test]
    fn out_of_bounds_carries_clamp_info() {
        let err = ToolError::OutOfBounds {
            original_range: [0.0, 99999.0],
            clamped_range: [0.0, 30000.0],
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error"], "out_of_bounds");
        assert_eq!(json["clamped_range"][1], 30000.0);
    }
}
```

- [ ] **Step 2: Run test (it should fail because `ToolError` doesn't exist)**

Run: `cargo test --lib error::tests`
Expected: FAIL with "cannot find type `ToolError`".

- [ ] **Step 3: Implement**

Replace `src/error.rs` content with:

```rust
//! Structured error envelope returned by every MCP tool.
//!
//! Wherever the LLM has enough information to retry, prefer warning + recovery
//! over a hard failure. See spec §"Error handling" for the full table.

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum ToolError {
    FileNotFound { path: PathBuf },
    NotAProfile { path: PathBuf, details: String },
    UnsupportedProfileFormat { path: PathBuf, version: String },
    FunctionNotFound { function: String, nearest_matches: Vec<String> },
    FunctionAmbiguous { function: String, candidates: Vec<FunctionCandidate> },
    ThreadNotFound { thread: String, available_threads: Vec<ThreadRef> },
    ProcessNotFound { process: String, available_processes: Vec<ProcessRef> },
    OutOfBounds { original_range: [f64; 2], clamped_range: [f64; 2] },
    ProfileNotFound { profile_id: String },
    ProfileEvicted { profile_id: String, original_path: PathBuf },
    Internal { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionCandidate {
    pub function: String,
    pub module: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadRef {
    pub tid: u64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessRef {
    pub pid: u64,
    pub name: String,
}

pub type ToolResult<T> = Result<T, ToolError>;

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string(self).unwrap_or_else(|_| "<error serialization failed>".into())
        )
    }
}

impl std::error::Error for ToolError {}
```

- [ ] **Step 4: Wire `error` into `main.rs`**

Replace `src/main.rs` content with:

```rust
mod error;

fn main() {
    println!("pollard placeholder");
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib error::tests`
Expected: PASS (2 passed).

- [ ] **Step 6: Verify clippy clean**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add src/error.rs src/main.rs
git commit -m "feat(error): add ToolError envelope with serde tag"
```

### Task 7: `matching.rs` — function matching

**Files:**
- Create: `src/matching.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write failing tests**

Append to `src/matching.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_matches_default() {
        let m = FunctionMatcher::new("malloc").unwrap();
        assert!(m.matches("malloc"));
        assert!(m.matches("_int_malloc"));
        assert!(m.matches("je_malloc"));
        assert!(!m.matches("free"));
    }

    #[test]
    fn regex_matches_with_re_prefix() {
        let m = FunctionMatcher::new("re:^memcpy_").unwrap();
        assert!(m.matches("memcpy_avx"));
        assert!(!m.matches("__memcpy"));
    }

    #[test]
    fn invalid_regex_returns_error() {
        let err = FunctionMatcher::new("re:[invalid").unwrap_err();
        assert!(err.to_string().contains("regex"));
    }

    #[test]
    fn case_sensitive() {
        let m = FunctionMatcher::new("Malloc").unwrap();
        assert!(!m.matches("malloc"));
    }
}
```

- [ ] **Step 2: Run tests (they should fail)**

Run: `cargo test --lib matching::tests`
Expected: FAIL — `FunctionMatcher` is undefined.

- [ ] **Step 3: Implement**

Top of `src/matching.rs`:

```rust
//! Function name matching: substring by default, regex with `re:` prefix.
//!
//! Used uniformly by every tool parameter that takes a function name
//! (`filter`, `function`, `root_function`, `paths_to`).

use regex::Regex;

#[derive(Debug)]
pub enum FunctionMatcher {
    Substring(String),
    Regex(Regex),
}

#[derive(Debug, thiserror::Error)]
pub enum MatcherError {
    #[error("invalid regex: {0}")]
    Regex(#[from] regex::Error),
}

impl FunctionMatcher {
    pub fn new(pattern: &str) -> Result<Self, MatcherError> {
        if let Some(re) = pattern.strip_prefix("re:") {
            Ok(Self::Regex(Regex::new(re)?))
        } else {
            Ok(Self::Substring(pattern.to_owned()))
        }
    }

    pub fn matches(&self, function_name: &str) -> bool {
        match self {
            Self::Substring(needle) => function_name.contains(needle),
            Self::Regex(re) => re.is_match(function_name),
        }
    }
}
```

Add `thiserror = "2"` to `[dependencies]` in `Cargo.toml`.

- [ ] **Step 4: Wire into `main.rs`**

Replace `src/main.rs`:

```rust
mod error;
mod matching;

fn main() {
    println!("pollard placeholder");
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib matching::tests`
Expected: PASS (4 passed).

- [ ] **Step 6: Verify clippy clean**

Run: `cargo clippy --all-targets -- -D warnings`

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml src/matching.rs src/main.rs
git commit -m "feat(matching): function name matcher with re: prefix"
```

---

## Phase 2 — Profile deserialization (partial)

The Firefox processed-profile format is documented at <https://github.com/firefox-devtools/profiler/blob/main/docs-developer/processed-profile-format.md>. We deserialize only the fields needed for v1 query tools. Each table is column-oriented (parallel arrays indexed by handle).

### Task 8: Raw deserialization types

**Files:**
- Create: `src/profile/mod.rs`
- Create: `src/profile/raw.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: `src/profile/mod.rs`**

```rust
//! Partial deserialization of the Firefox processed-profile JSON.
//!
//! We only deserialize the fields we need for v1 tools:
//! - lib table (for symbolication and module names)
//! - func table, frame table, stack table, sample table (for aggregation)
//! - resource table, string array (for name resolution)
//! - thread/process metadata
//!
//! Markers, counters, profiler config, and other top-level fields are skipped.

pub mod load;
pub mod parsed;
pub mod raw;

pub use load::load_from_path;
pub use parsed::{Profile, ProcessHandle, ThreadHandle};
```

- [ ] **Step 2: Write failing tests for raw deserialization**

Append to `src/profile/raw.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
        "libs": [],
        "threads": [{
            "name": "Main",
            "tid": 1,
            "pid": 1,
            "registerTime": 0.0,
            "stringArray": ["foo", "bar"],
            "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "innerWindowID": [], "implementation": [], "line": [], "column": [], "nativeSymbol": []},
            "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
            "samples": {"length": 0, "stack": [], "time": [], "weight": null, "weightType": "samples"},
            "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
            "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
            "markers": {"length": 0, "data": [], "name": [], "startTime": [], "endTime": [], "phase": [], "category": []},
            "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []},
            "processType": "default",
            "processStartupTime": 0.0
        }]
    }"#;

    #[test]
    fn deserializes_minimal_profile() {
        let p: RawProfile = serde_json::from_str(MINIMAL).unwrap();
        assert_eq!(p.threads.len(), 1);
        assert_eq!(p.threads[0].name.as_deref(), Some("Main"));
    }

    #[test]
    fn extra_fields_are_ignored() {
        let with_extras = MINIMAL.replace(
            r#""product": "test"#,
            r#""unknownField": 42, "product": "test"#,
        );
        serde_json::from_str::<RawProfile>(&with_extras).unwrap();
    }
}
```

- [ ] **Step 3: Run tests (they should fail)**

Run: `cargo test --lib profile::raw::tests`
Expected: FAIL — `RawProfile` undefined.

- [ ] **Step 4: Implement raw types**

Replace top of `src/profile/raw.rs`:

```rust
//! Raw serde-derived structs that match the Firefox processed-profile JSON.
//! Field naming uses serde-rename to convert from camelCase JSON.
//!
//! These types intentionally do NOT cover every field in the schema — only
//! what v1 query tools touch. Unknown fields are ignored.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawProfile {
    pub meta: RawMeta,
    #[serde(default)]
    pub libs: Vec<RawLib>,
    #[serde(default)]
    pub threads: Vec<RawThread>,
    #[serde(default)]
    pub processes: Vec<RawProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawMeta {
    pub interval: f64,
    pub start_time: f64,
    #[serde(default)]
    pub product: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawLib {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub debug_name: Option<String>,
    #[serde(default)]
    pub debug_path: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub breakpad_id: Option<String>,
    #[serde(default)]
    pub code_id: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThread {
    pub tid: u64,
    pub pid: u64,
    #[serde(default)]
    pub name: Option<String>,
    pub register_time: f64,
    pub string_array: Vec<String>,
    pub frame_table: RawFrameTable,
    pub func_table: RawFuncTable,
    pub stack_table: RawStackTable,
    pub samples: RawSampleTable,
    pub resource_table: RawResourceTable,
    #[serde(default)]
    pub native_symbols: Option<RawNativeSymbols>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFrameTable {
    pub length: usize,
    pub address: Vec<i64>,           // -1 for non-native
    pub func: Vec<usize>,
    pub line: Vec<Option<u32>>,
    pub column: Vec<Option<u32>>,
    pub category: Vec<Option<usize>>,
    pub subcategory: Vec<Option<usize>>,
    #[serde(default)]
    pub native_symbol: Vec<Option<usize>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFuncTable {
    pub length: usize,
    pub name: Vec<usize>,            // string-array index
    pub is_js: Vec<bool>,
    pub relevant_for_js: Vec<bool>,
    pub resource: Vec<i32>,          // -1 if no resource
    pub file_name: Vec<Option<usize>>, // string-array index
    pub line_number: Vec<Option<u32>>,
    pub column_number: Vec<Option<u32>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStackTable {
    pub length: usize,
    pub frame: Vec<usize>,
    pub category: Vec<usize>,
    pub subcategory: Vec<usize>,
    pub prefix: Vec<Option<usize>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSampleTable {
    pub length: usize,
    pub stack: Vec<Option<usize>>,
    pub time: Vec<f64>,
    #[serde(default)]
    pub weight: Option<Vec<f64>>,
    #[serde(default)]
    pub weight_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawResourceTable {
    pub length: usize,
    pub lib: Vec<Option<usize>>,
    pub name: Vec<usize>,            // string-array index
    pub host: Vec<Option<usize>>,
    #[serde(rename = "type")]
    pub type_: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawNativeSymbols {
    pub length: usize,
    pub lib_index: Vec<usize>,
    pub address: Vec<i64>,
    pub name: Vec<usize>,            // string-array index
    pub function_size: Vec<Option<u64>>,
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib profile::raw::tests`
Expected: PASS.

- [ ] **Step 6: Wire into `main.rs`**

```rust
mod error;
mod matching;
mod profile;

fn main() {
    println!("pollard placeholder");
}
```

- [ ] **Step 7: Verify clippy clean**

Run: `cargo clippy --all-targets -- -D warnings`

- [ ] **Step 8: Commit**

```bash
git add src/profile src/main.rs
git commit -m "feat(profile): partial raw deserialization of Firefox profile JSON"
```

### Task 9: Loading `.json` and `.json.gz`

**Files:**
- Create: `src/profile/load.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/profile/load.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    const MINIMAL: &str = include_str!("../../tests/fixtures/minimal_profile.json");

    #[test]
    fn loads_uncompressed_json() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        f.write_all(MINIMAL.as_bytes()).unwrap();
        let p = load_from_path(f.path()).unwrap();
        assert!(!p.threads.is_empty());
    }

    #[test]
    fn loads_gzipped_json() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut f = NamedTempFile::with_suffix(".json.gz").unwrap();
        let mut gz = GzEncoder::new(f.as_file_mut(), Compression::default());
        gz.write_all(MINIMAL.as_bytes()).unwrap();
        gz.finish().unwrap();
        let p = load_from_path(f.path()).unwrap();
        assert!(!p.threads.is_empty());
    }

    #[test]
    fn missing_file_returns_file_not_found() {
        let err = load_from_path(std::path::Path::new("/no/such/file.json")).unwrap_err();
        assert!(matches!(err, crate::error::ToolError::FileNotFound { .. }));
    }
}
```

- [ ] **Step 2: Create the fixture**

Create `tests/fixtures/minimal_profile.json` with the same content as the `MINIMAL` constant from Task 8 (single thread, empty tables).

- [ ] **Step 3: Run tests (they should fail)**

Run: `cargo test --lib profile::load::tests`
Expected: FAIL — `load_from_path` undefined.

- [ ] **Step 4: Implement**

```rust
//! Read a profile file (.json or .json.gz) into our raw types.

use crate::error::ToolError;
use crate::profile::raw::RawProfile;
use flate2::bufread::GzDecoder;
use std::ffi::OsStr;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

pub fn load_from_path(path: &Path) -> Result<RawProfile, ToolError> {
    let file = File::open(path).map_err(|_| ToolError::FileNotFound {
        path: path.to_path_buf(),
    })?;
    let reader = BufReader::new(file);

    if path.extension() == Some(OsStr::new("gz")) {
        let decoder = GzDecoder::new(reader);
        let reader = BufReader::new(decoder);
        serde_json::from_reader(reader).map_err(|e| ToolError::NotAProfile {
            path: path.to_path_buf(),
            details: e.to_string(),
        })
    } else {
        serde_json::from_reader(reader).map_err(|e| ToolError::NotAProfile {
            path: path.to_path_buf(),
            details: e.to_string(),
        })
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib profile::load::tests`
Expected: PASS (3 passed).

- [ ] **Step 6: Verify clippy clean**

Run: `cargo clippy --all-targets -- -D warnings`

- [ ] **Step 7: Commit**

```bash
git add src/profile/load.rs tests/fixtures
git commit -m "feat(profile): load .json and .json.gz files into raw types"
```

### Task 10: Parsed `Profile` accessor layer

**Files:**
- Create: `src/profile/parsed.rs`

The raw types are column-oriented. The query layer wants ergonomic, row-oriented access: "give me frame F's function name, file, line, module." This task builds a `Profile` wrapper that owns the raw data and provides typed accessors.

- [ ] **Step 1: Write failing tests**

Append to `src/profile/parsed.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::RawProfile;

    fn fixture() -> Profile {
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/minimal_profile.json"
        ))
        .unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn enumerates_threads() {
        let p = fixture();
        let threads: Vec<_> = p.threads().collect();
        assert_eq!(threads.len(), 1);
        let t = &threads[0];
        assert_eq!(t.tid(), 1);
        assert_eq!(t.name(), Some("Main"));
    }

    #[test]
    fn duration_ms_is_zero_for_empty_profile() {
        let p = fixture();
        assert_eq!(p.duration_ms(), 0.0);
    }
}
```

- [ ] **Step 2: Run tests (fail)**

Run: `cargo test --lib profile::parsed::tests`
Expected: FAIL — `Profile`, `from_raw` undefined.

- [ ] **Step 3: Implement**

```rust
//! Ergonomic accessors over the raw profile tables.
//!
//! This layer owns the `RawProfile` and exposes:
//! - threads + processes (with tid/pid/name)
//! - frame lookup: `Profile::frame_info(thread_handle, frame_index)` →
//!   `{function_name, module_name, file?, line?, address?}`
//! - stack walking: iterate samples and walk their stack chains
//! - duration / sample rate
//!
//! Keep this read-only and `Sync`; query functions must be able to share an
//! `&Profile` across threads.

use crate::profile::raw::{RawLib, RawProfile, RawThread};

pub struct Profile {
    raw: RawProfile,
    /// Flattened (process, thread) tuples for top-level enumeration.
    threads: Vec<ThreadHandle>,
}

#[derive(Clone, Copy, Debug)]
pub struct ProcessHandle {
    pub pid: u64,
    pub process_idx: Option<usize>, // None means "root profile is itself a process"
}

#[derive(Clone, Copy, Debug)]
pub struct ThreadHandle {
    pub process: ProcessHandle,
    pub thread_idx: usize,
}

#[derive(Debug, Clone)]
pub struct FrameInfo<'a> {
    pub function_name: &'a str,
    pub module_name: Option<&'a str>,
    pub file: Option<&'a str>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub address: Option<i64>,
}

pub struct ThreadView<'a> {
    profile: &'a Profile,
    handle: ThreadHandle,
}

impl Profile {
    pub fn from_raw(raw: RawProfile) -> Self {
        let mut threads = Vec::new();
        // Top-level threads belong to the implicit "root" process.
        for (i, _) in raw.threads.iter().enumerate() {
            threads.push(ThreadHandle {
                process: ProcessHandle { pid: 0, process_idx: None },
                thread_idx: i,
            });
        }
        // Sub-process threads.
        for (pi, p) in raw.processes.iter().enumerate() {
            for (i, t) in p.threads.iter().enumerate() {
                threads.push(ThreadHandle {
                    process: ProcessHandle {
                        pid: t.pid,
                        process_idx: Some(pi),
                    },
                    thread_idx: i,
                });
            }
        }
        Self { raw, threads }
    }

    pub fn meta(&self) -> &crate::profile::raw::RawMeta {
        &self.raw.meta
    }

    pub fn threads(&self) -> impl Iterator<Item = ThreadView<'_>> + '_ {
        self.threads.iter().map(move |&h| ThreadView { profile: self, handle: h })
    }

    pub fn duration_ms(&self) -> f64 {
        self.threads()
            .filter_map(|t| {
                let raw = t.raw();
                let times = &raw.samples.time;
                Some(*times.last()? - *times.first()?)
            })
            .fold(0.0_f64, f64::max)
    }

    /// Resolve a thread handle back to the raw thread.
    pub(crate) fn raw_thread(&self, handle: ThreadHandle) -> &RawThread {
        match handle.process.process_idx {
            None => &self.raw.threads[handle.thread_idx],
            Some(pi) => &self.raw.processes[pi].threads[handle.thread_idx],
        }
    }

    /// Look up the lib for a `RawResourceTable.lib` index.
    pub(crate) fn lib(&self, idx: usize) -> Option<&RawLib> {
        self.raw.libs.get(idx)
    }

    /// Look up frame info for a given thread + frame index.
    pub fn frame_info(&self, handle: ThreadHandle, frame_idx: usize) -> Option<FrameInfo<'_>> {
        let thread = self.raw_thread(handle);
        let func_idx = *thread.frame_table.func.get(frame_idx)?;
        let func_name_idx = *thread.func_table.name.get(func_idx)?;
        let function_name = thread.string_array.get(func_name_idx)?.as_str();

        let resource_idx = thread.func_table.resource.get(func_idx).copied().unwrap_or(-1);
        let module_name = if resource_idx >= 0 {
            let r = thread.resource_table.lib.get(resource_idx as usize)?;
            r.and_then(|li| self.lib(li))
                .and_then(|l| l.name.as_deref())
        } else {
            None
        };

        let file = thread.func_table.file_name.get(func_idx).and_then(|opt| {
            opt.and_then(|si| thread.string_array.get(si).map(String::as_str))
        });

        let line = thread.frame_table.line.get(frame_idx).copied().flatten();
        let column = thread.frame_table.column.get(frame_idx).copied().flatten();
        let address = thread.frame_table.address.get(frame_idx).copied();
        let address = address.filter(|&a| a >= 0);

        Some(FrameInfo { function_name, module_name, file, line, column, address })
    }

    /// Walk the frame indices for a stack from leaf to root.
    pub fn walk_stack<'a>(
        &'a self,
        handle: ThreadHandle,
        stack_idx: usize,
    ) -> impl Iterator<Item = usize> + 'a {
        let thread = self.raw_thread(handle);
        let mut current = Some(stack_idx);
        std::iter::from_fn(move || {
            let s = current?;
            let frame = *thread.stack_table.frame.get(s)?;
            current = thread.stack_table.prefix.get(s).copied().flatten();
            Some(frame)
        })
    }
}

impl<'a> ThreadView<'a> {
    pub fn handle(&self) -> ThreadHandle {
        self.handle
    }

    pub fn raw(&self) -> &'a RawThread {
        self.profile.raw_thread(self.handle)
    }

    pub fn tid(&self) -> u64 {
        self.raw().tid
    }

    pub fn pid(&self) -> u64 {
        self.raw().pid
    }

    pub fn name(&self) -> Option<&'a str> {
        self.raw().name.as_deref()
    }

    pub fn samples(&self) -> &'a crate::profile::raw::RawSampleTable {
        &self.raw().samples
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib profile::parsed::tests`
Expected: PASS (2 passed).

- [ ] **Step 5: Verify clippy clean**

- [ ] **Step 6: Commit**

```bash
git add src/profile/parsed.rs
git commit -m "feat(profile): parsed accessor layer"
```

### Task 11: Synthetic profile builder for tests

**Files:**
- Create: `tests/helpers/mod.rs`
- Create: `tests/helpers/synthetic.rs`

We use `fxprof-processed-profile` to *write* small profiles, then deserialize them with our raw types. This gives test code a fluent builder ("thread T has these stacks with these counts") without hand-writing JSON.

- [ ] **Step 1: Write the helper**

`tests/helpers/mod.rs`:

```rust
pub mod synthetic;
```

`tests/helpers/synthetic.rs`:

```rust
//! Build small profiles for tests using fxprof-processed-profile, then
//! serialize+deserialize them through our raw types.

use fxprof_processed_profile::{
    CategoryHandle, CpuDelta, FrameFlags, Profile as FxProfile, SamplingInterval, Timestamp,
};
use std::time::SystemTime;

/// Stack helper: each entry is (function_name, module_label).
pub struct SampleSpec<'a> {
    pub stack: &'a [(&'a str, &'a str)],
    pub count: u32,
}

/// Build a single-thread profile with the given samples.
pub fn build_simple_profile(name: &str, samples: &[SampleSpec<'_>]) -> String {
    let mut profile = FxProfile::new(
        name,
        SystemTime::now().into(),
        SamplingInterval::from_millis(1),
    );
    let process = profile.add_process("test-process", 1, Timestamp::from_millis_since_reference(0.0));
    let thread = profile.add_thread(process, 1, Timestamp::from_millis_since_reference(0.0), true);
    profile.set_thread_name(thread, "Main");

    let mut t = 0.0_f64;
    for sample in samples {
        let mut current_stack = None;
        for (fn_name, _module) in sample.stack {
            let s_handle = profile.handle_for_string(fn_name);
            let frame = profile.handle_for_frame_with_label(
                s_handle,
                CategoryHandle::OTHER,
                FrameFlags::empty(),
            );
            current_stack = Some(profile.handle_for_stack(frame, current_stack));
        }
        for _ in 0..sample.count {
            profile.add_sample(
                thread,
                Timestamp::from_millis_since_reference(t),
                current_stack,
                CpuDelta::ZERO,
                1,
            );
            t += 1.0;
        }
    }

    serde_json::to_string(&profile).unwrap()
}
```

(Module names are not currently set on synthetic profiles — pass them anyway so the API is forward-compatible; tests that need module info should use a fixture profile recorded with `samply` or extend this helper.)

- [ ] **Step 2: Smoke test (no separate test step — used by later tasks)**

This is a helper module only; later test tasks will exercise it.

- [ ] **Step 3: Commit**

```bash
git add tests/helpers
git commit -m "test: add synthetic profile builder helper"
```

---

## Phase 3 — ProfileSession

### Task 12: `ProfileSession` — load + symbolicate handle

**Files:**
- Create: `src/session.rs`
- Modify: `src/main.rs`

Symbolication is delegated to `wholesym::SymbolManager`. The session owns it and the parsed `Profile`. For v1, `unsymbolicated_pct` is computed naively as the fraction of frames whose `module_name` is `None`; refining symbolication semantics happens later.

- [ ] **Step 1: Write failing tests**

Append to `src/session.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loads_and_describes_synthetic_profile() {
        // Reuses the test helper module via a dev-dep path workaround:
        // since helpers/ is in tests/, this unit test inlines a minimal
        // builder. Fuller scenarios live in integration tests.
        let raw_json = include_str!("../tests/fixtures/minimal_profile.json");
        let tmp = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        std::fs::write(tmp.path(), raw_json).unwrap();
        let session = ProfileSession::load(tmp.path(), Some("test")).await.unwrap();
        assert_eq!(session.name(), "test");
        assert!(session.profile().threads().count() >= 1);
    }
}
```

- [ ] **Step 2: Run tests (fail)**

Run: `cargo test --lib session::tests`
Expected: FAIL — `ProfileSession` undefined.

- [ ] **Step 3: Implement**

```rust
//! A loaded, symbolicated profile, ready to query.

use crate::error::ToolError;
use crate::profile::{Profile, load_from_path};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ProfileSession {
    id: String,
    name: String,
    path: PathBuf,
    profile: Arc<Profile>,
    /// Fraction of frames that did not symbolicate. 0.0–100.0.
    unsymbolicated_pct: f32,
}

impl ProfileSession {
    pub async fn load(path: &Path, name: Option<&str>) -> Result<Self, ToolError> {
        let abs = path
            .canonicalize()
            .map_err(|_| ToolError::FileNotFound { path: path.to_path_buf() })?;

        let raw = load_from_path(&abs)?;
        let profile = Arc::new(Profile::from_raw(raw));

        // For v1 we treat the profile as already-symbolicated by samply itself
        // (samply runs symbolication during recording). `wholesym` integration
        // for re-symbolicating an unsymbolicated profile is deferred — see spec
        // §"Architecture: Failure surface vs. samply".
        let unsymbolicated_pct = compute_unsymbolicated_pct(&profile);

        let id = profile_id_from_path(&abs);
        let name = name
            .map(str::to_owned)
            .unwrap_or_else(|| {
                abs.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.trim_end_matches(".json").to_owned())
                    .unwrap_or_else(|| id.clone())
            });

        Ok(Self {
            id,
            name,
            path: abs,
            profile,
            unsymbolicated_pct,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn shared_profile(&self) -> Arc<Profile> {
        self.profile.clone()
    }

    pub fn unsymbolicated_pct(&self) -> f32 {
        self.unsymbolicated_pct
    }
}

fn profile_id_from_path(path: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    let v = h.finish();
    format!("{:08x}", v as u32)
}

fn compute_unsymbolicated_pct(profile: &Profile) -> f32 {
    let mut total: u64 = 0;
    let mut unsymbolicated: u64 = 0;
    for thread in profile.threads() {
        let handle = thread.handle();
        let raw = thread.raw();
        for s in raw.samples.stack.iter().flatten() {
            for frame_idx in profile.walk_stack(handle, *s) {
                total += 1;
                let info = profile.frame_info(handle, frame_idx);
                let is_unsym = info
                    .as_ref()
                    .is_none_or(|i| i.function_name.is_empty() || i.function_name == "0x0");
                if is_unsym {
                    unsymbolicated += 1;
                }
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        100.0 * unsymbolicated as f32 / total as f32
    }
}
```

Add `tempfile = "3"` to `[dev-dependencies]` (already there) and `tokio = { ..., features = [..., "macros"] }` already configured.

- [ ] **Step 4: Wire into `main.rs`**

```rust
mod error;
mod matching;
mod profile;
mod session;

fn main() {
    println!("pollard placeholder");
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib session::tests`
Expected: PASS.

- [ ] **Step 6: Verify clippy clean**

- [ ] **Step 7: Commit**

```bash
git add src/session.rs src/main.rs
git commit -m "feat(session): ProfileSession with id, path, and unsymbolicated_pct"
```

### Task 13: `describe_profile` query

**Files:**
- Create: `src/query/mod.rs`
- Create: `src/query/describe.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write failing test**

Append to `src/query/describe.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    #[test]
    fn describes_minimal_profile() {
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/minimal_profile.json"
        ))
        .unwrap();
        let profile = Profile::from_raw(raw);
        let desc = describe(&profile, "id1", "name1", "/tmp/p.json", 0.0);
        assert_eq!(desc.profile_id, "id1");
        assert_eq!(desc.name, "name1");
        assert_eq!(desc.unsymbolicated_pct, 0.0);
        assert!(!desc.processes.is_empty() || !desc.processes.is_empty());
    }
}
```

- [ ] **Step 2: Run (fail)**

Run: `cargo test --lib query::describe::tests`
Expected: FAIL — `describe` undefined.

- [ ] **Step 3: Implement**

`src/query/mod.rs`:

```rust
pub mod describe;
```

`src/query/describe.rs`:

```rust
//! Implementation of the `describe_profile` MCP tool.

use crate::profile::Profile;
use serde::Serialize;

#[derive(Serialize, Debug)]
pub struct ProfileDescription {
    pub profile_id: String,
    pub name: String,
    pub path: String,
    pub duration_ms: f64,
    pub sample_rate_hz: f64,
    pub total_samples: u64,
    pub unsymbolicated_pct: f32,
    pub processes: Vec<ProcessDescription>,
}

#[derive(Serialize, Debug)]
pub struct ProcessDescription {
    pub pid: u64,
    pub name: String,
    pub thread_count: usize,
    pub threads: Vec<ThreadDescription>,
}

#[derive(Serialize, Debug)]
pub struct ThreadDescription {
    pub tid: u64,
    pub name: String,
    pub samples: u64,
    pub duration_ms: f64,
}

pub fn describe(
    profile: &Profile,
    profile_id: &str,
    name: &str,
    path: &str,
    unsymbolicated_pct: f32,
) -> ProfileDescription {
    let interval_ms = profile.meta().interval;
    let sample_rate_hz = if interval_ms > 0.0 { 1000.0 / interval_ms } else { 0.0 };

    // Group threads by pid.
    let mut by_pid: std::collections::BTreeMap<u64, Vec<ThreadDescription>> =
        std::collections::BTreeMap::new();
    let mut total_samples: u64 = 0;

    for thread in profile.threads() {
        let raw = thread.raw();
        let times = &raw.samples.time;
        let dur = times.last().copied().unwrap_or(0.0) - times.first().copied().unwrap_or(0.0);
        let samples = raw.samples.length as u64;
        total_samples += samples;
        by_pid.entry(thread.pid()).or_default().push(ThreadDescription {
            tid: thread.tid(),
            name: thread.name().unwrap_or("").to_owned(),
            samples,
            duration_ms: dur,
        });
    }

    let processes = by_pid
        .into_iter()
        .map(|(pid, threads)| ProcessDescription {
            pid,
            name: String::new(), // TODO: extract from RawProfile.processes when present
            thread_count: threads.len(),
            threads,
        })
        .collect();

    ProfileDescription {
        profile_id: profile_id.to_owned(),
        name: name.to_owned(),
        path: path.to_owned(),
        duration_ms: profile.duration_ms(),
        sample_rate_hz,
        total_samples,
        unsymbolicated_pct,
        processes,
    }
}
```

(Note the `TODO`-ish comment — this is acceptable at the *implementation* level when a follow-up is genuinely scoped; `process_name` is not load-bearing for v1 tools. Remove the comment once the per-process name is wired.)

- [ ] **Step 4: Wire into `main.rs`**

```rust
mod error;
mod matching;
mod profile;
mod query;
mod session;

fn main() {
    println!("pollard placeholder");
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib query::describe::tests`
Expected: PASS.

- [ ] **Step 6: Verify clippy clean**

- [ ] **Step 7: Commit**

```bash
git add src/query src/main.rs
git commit -m "feat(query): describe_profile output type"
```

---

## Phase 4 — `top_functions`

### Task 14: filters (thread, process, time_range)

**Files:**
- Create: `src/query/filters.rs`
- Modify: `src/query/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to `src/query/filters.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn fixture() -> Profile {
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/minimal_profile.json"
        ))
        .unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn no_filter_keeps_everything() {
        let p = fixture();
        let filter = Filter::default();
        let kept: Vec<_> = filter.threads(&p).collect();
        assert!(!kept.is_empty());
    }

    #[test]
    fn thread_name_filter_matches() {
        let p = fixture();
        let filter = Filter {
            thread: Some(ThreadFilter::Name("Main".into())),
            ..Default::default()
        };
        let kept: Vec<_> = filter.threads(&p).collect();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn unmatched_thread_returns_empty() {
        let p = fixture();
        let filter = Filter {
            thread: Some(ThreadFilter::Name("Nope".into())),
            ..Default::default()
        };
        let kept: Vec<_> = filter.threads(&p).collect();
        assert!(kept.is_empty());
    }
}
```

- [ ] **Step 2: Run tests (fail)**

Run: `cargo test --lib query::filters::tests`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! Reusable filter abstraction for thread/process/time selection.

use crate::error::{ProcessRef, ThreadRef, ToolError};
use crate::profile::{Profile, ThreadHandle};

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub thread: Option<ThreadFilter>,
    pub process: Option<ProcessFilter>,
    pub time_range: Option<[f64; 2]>,
}

#[derive(Debug, Clone)]
pub enum ThreadFilter {
    Tid(u64),
    Name(String),
}

#[derive(Debug, Clone)]
pub enum ProcessFilter {
    Pid(u64),
    Name(String),
}

impl Filter {
    /// Returns thread handles matching the filter. Empty if nothing matches.
    pub fn threads<'a>(&'a self, profile: &'a Profile) -> impl Iterator<Item = ThreadHandle> + 'a {
        profile.threads().filter_map(move |t| {
            if let Some(pf) = &self.process {
                let ok = match pf {
                    ProcessFilter::Pid(p) => t.pid() == *p,
                    ProcessFilter::Name(_) => false, // TODO: wire process names
                };
                if !ok {
                    return None;
                }
            }
            if let Some(tf) = &self.thread {
                let ok = match tf {
                    ThreadFilter::Tid(tid) => t.tid() == *tid,
                    ThreadFilter::Name(n) => t.name().is_some_and(|name| name == n),
                };
                if !ok {
                    return None;
                }
            }
            Some(t.handle())
        })
    }

    /// Validate thread filter; if it matches no threads, return a structured error.
    pub fn validate_thread(&self, profile: &Profile) -> Result<(), ToolError> {
        if self.thread.is_none() {
            return Ok(());
        }
        if self.threads(profile).next().is_some() {
            return Ok(());
        }
        let available_threads = profile
            .threads()
            .map(|t| ThreadRef {
                tid: t.tid(),
                name: t.name().unwrap_or("").to_owned(),
            })
            .collect();
        let thread = match self.thread.as_ref().unwrap() {
            ThreadFilter::Tid(t) => t.to_string(),
            ThreadFilter::Name(n) => n.clone(),
        };
        Err(ToolError::ThreadNotFound { thread, available_threads })
    }

    /// Clamp a time range to the profile's actual duration; emit no error.
    /// Returns the clamped range and the original-range diagnostic if anything changed.
    pub fn clamped_time_range(
        &self,
        profile_duration: f64,
    ) -> Option<(([f64; 2]), Option<[f64; 2]>)> {
        let r = self.time_range?;
        let clamped = [r[0].max(0.0), r[1].min(profile_duration)];
        let changed = if (clamped[0] - r[0]).abs() > f64::EPSILON
            || (clamped[1] - r[1]).abs() > f64::EPSILON
        {
            Some(r)
        } else {
            None
        };
        Some((clamped, changed))
    }
}

// (also add ProcessRef for parity with ThreadRef use sites)
fn _unused() -> ProcessRef {
    ProcessRef { pid: 0, name: String::new() }
}
```

Update `src/query/mod.rs`:

```rust
pub mod describe;
pub mod filters;
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib query::filters::tests`
Expected: PASS.

- [ ] **Step 5: Remove the placeholder `_unused`**

That dead helper exists only to keep `ProcessRef` referenced during drafting — drop it once a real consumer (e.g. `validate_process`) is added in a later task. For now, allow the warning by importing `ProcessRef` only behind `#[allow(dead_code)]` or simply moving the import to `pub use`. Choose the import that keeps clippy clean (we use `-D warnings`).

The cleanest fix:

```rust
pub use crate::error::ProcessRef;  // delete _unused() and the ProcessRef import line
```

Or leave the validator stubbed out for `validate_process` (added in the task that needs it).

- [ ] **Step 6: Verify clippy clean**

Run: `cargo clippy --all-targets -- -D warnings`

- [ ] **Step 7: Commit**

```bash
git add src/query
git commit -m "feat(query): thread/process/time filter"
```

### Task 15: `top_functions` implementation

**Files:**
- Create: `src/query/top_functions.rs`
- Modify: `src/query/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to `src/query/top_functions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn raw_with_two_functions() -> RawProfile {
        // 90 samples in `hot()`, 10 in `cold()`.
        // Hand-build a minimal profile by splicing string array, func table, frame table, etc.
        // (Easier to use the synthetic builder helper from tests/helpers — but that's only
        // available to integration tests. For this unit test, deserialize a checked-in
        // fixture.)
        serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap()
    }

    #[test]
    fn ranks_by_self_samples() {
        let profile = Profile::from_raw(raw_with_two_functions());
        let result = top_functions(&profile, &Args::default()).unwrap();
        assert_eq!(result.functions[0].function, "hot");
        assert_eq!(result.functions[0].self_samples, 90);
        assert_eq!(result.functions[1].function, "cold");
        assert_eq!(result.functions[1].self_samples, 10);
    }

    #[test]
    fn limit_truncates() {
        let profile = Profile::from_raw(raw_with_two_functions());
        let result = top_functions(&profile, &Args { limit: 1, ..Default::default() }).unwrap();
        assert_eq!(result.functions.len(), 1);
    }

    #[test]
    fn filter_substring_restricts() {
        let profile = Profile::from_raw(raw_with_two_functions());
        let result = top_functions(&profile, &Args { filter: Some("hot".into()), ..Default::default() }).unwrap();
        assert_eq!(result.functions.len(), 1);
        assert_eq!(result.functions[0].function, "hot");
    }
}
```

- [ ] **Step 2: Create `tests/fixtures/two_functions.json`**

Hand-build a minimal profile JSON with two functions. The simplest way is to write a tiny Rust binary in `tests/build_fixtures.rs` that uses `fxprof-processed-profile` to build the profile, then write the resulting JSON to a file via:

```bash
cargo test --test build_fixtures -- --ignored --exact build_two_functions_fixture
```

Or hand-write the JSON. Use the `synthetic.rs` helper if practical; otherwise, hand-author a 50-line JSON document that:
- has 1 thread, 1 process
- string array: `["hot", "cold"]`
- func table: 2 funcs (hot=0, cold=1) referencing string indices 0, 1
- frame table: 2 frames (frame 0 → func 0, frame 1 → func 1)
- stack table: 2 stacks (stack 0 → frame 0, stack 1 → frame 1)
- sample table: 100 samples — 90 referencing stack 0, 10 referencing stack 1

Save this exactly under `tests/fixtures/two_functions.json`. (The plan does not inline the JSON for brevity, but the implementing engineer must write the exact content; treat this as a step that fails until the fixture exists.)

- [ ] **Step 3: Run tests (fail)**

Run: `cargo test --lib query::top_functions::tests`
Expected: FAIL — `top_functions` undefined.

- [ ] **Step 4: Implement**

```rust
//! `top_functions` aggregation: flat top-N by self or total samples.

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::{Profile, ThreadHandle};
use crate::query::filters::Filter;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub filter: Option<String>,
    pub limit: usize,
    pub sort_by: SortBy,
    pub filter_args: Filter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    SelfTime,
    TotalTime,
}

#[derive(Debug, Serialize)]
pub struct Output {
    pub thread: Option<String>,
    pub process: Option<String>,
    pub total_samples: u64,
    pub filter: Option<String>,
    pub sort_by: &'static str,
    pub functions: Vec<FunctionEntry>,
}

#[derive(Debug, Serialize)]
pub struct FunctionEntry {
    pub rank: usize,
    pub function: String,
    pub module: Option<String>,
    pub self_samples: u64,
    pub self_pct: f32,
    pub total_samples: u64,
    pub total_pct: f32,
}

const DEFAULT_LIMIT: usize = 30;

#[derive(Default)]
struct Counts {
    self_samples: u64,
    total_samples: u64,
}

pub fn top_functions(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    args.filter_args.validate_thread(profile)?;
    let matcher = match args.filter.as_deref() {
        Some(p) => Some(
            FunctionMatcher::new(p).map_err(|e| ToolError::Internal { message: e.to_string() })?,
        ),
        None => None,
    };

    let mut counts: HashMap<(String, Option<String>), Counts> = HashMap::new();
    let mut total_samples: u64 = 0;

    for handle in args.filter_args.threads(profile) {
        accumulate_thread(profile, handle, args.sort_by, &matcher, &mut counts, &mut total_samples);
    }

    // Build output
    let mut entries: Vec<((String, Option<String>), Counts)> = counts.into_iter().collect();
    entries.sort_by(|a, b| {
        let ka = match args.sort_by {
            SortBy::SelfTime => a.1.self_samples,
            SortBy::TotalTime => a.1.total_samples,
        };
        let kb = match args.sort_by {
            SortBy::SelfTime => b.1.self_samples,
            SortBy::TotalTime => b.1.total_samples,
        };
        kb.cmp(&ka)
            .then_with(|| a.0.0.cmp(&b.0.0))
            .then_with(|| a.0.1.cmp(&b.0.1))
    });

    let limit = if args.limit == 0 { DEFAULT_LIMIT } else { args.limit };
    let total = total_samples.max(1) as f32;
    let functions: Vec<_> = entries
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(i, ((function, module), c))| FunctionEntry {
            rank: i + 1,
            function,
            module,
            self_samples: c.self_samples,
            self_pct: 100.0 * c.self_samples as f32 / total,
            total_samples: c.total_samples,
            total_pct: 100.0 * c.total_samples as f32 / total,
        })
        .collect();

    Ok(Output {
        thread: None,
        process: None,
        total_samples,
        filter: args.filter.clone(),
        sort_by: match args.sort_by {
            SortBy::SelfTime => "self",
            SortBy::TotalTime => "total",
        },
        functions,
    })
}

fn accumulate_thread(
    profile: &Profile,
    handle: ThreadHandle,
    _sort_by: SortBy,
    matcher: &Option<FunctionMatcher>,
    counts: &mut HashMap<(String, Option<String>), Counts>,
    total_samples: &mut u64,
) {
    let raw = profile.raw_thread(handle);
    for &stack_opt in &raw.samples.stack {
        let Some(stack_idx) = stack_opt else { continue };
        *total_samples += 1;
        // Walk leaf-to-root. Self-time = leaf frame's function.
        let mut frames = profile.walk_stack(handle, stack_idx);
        let mut seen_in_stack: std::collections::HashSet<(String, Option<String>)> = Default::default();
        if let Some(leaf_frame_idx) = frames.next() {
            if let Some(info) = profile.frame_info(handle, leaf_frame_idx) {
                if matcher.as_ref().is_none_or(|m| m.matches(info.function_name)) {
                    let key = (info.function_name.to_owned(), info.module_name.map(str::to_owned));
                    counts.entry(key.clone()).or_default().self_samples += 1;
                    counts.entry(key.clone()).or_default().total_samples += 1;
                    seen_in_stack.insert(key);
                }
            }
        }
        // Total-time: increment for each unique (function, module) in the stack
        // (above the leaf — leaf is already counted).
        for frame_idx in frames {
            if let Some(info) = profile.frame_info(handle, frame_idx) {
                if matcher.as_ref().is_none_or(|m| m.matches(info.function_name)) {
                    let key = (info.function_name.to_owned(), info.module_name.map(str::to_owned));
                    if seen_in_stack.insert(key.clone()) {
                        counts.entry(key).or_default().total_samples += 1;
                    }
                }
            }
        }
    }
}
```

Update `src/query/mod.rs`:

```rust
pub mod describe;
pub mod filters;
pub mod top_functions;
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib query::top_functions::tests`
Expected: PASS.

- [ ] **Step 6: Verify clippy clean**

- [ ] **Step 7: Commit**

```bash
git add src/query/top_functions.rs src/query/mod.rs tests/fixtures/two_functions.json
git commit -m "feat(query): top_functions aggregation"
```

---

## Phase 5 — `call_tree`

The biggest aggregation in this project. Built up incrementally.

### Task 16: Tree construction (no pruning)

**Files:**
- Create: `src/query/call_tree.rs`
- Modify: `src/query/mod.rs`

- [ ] **Step 1: Write failing test for unpruned tree**

Append to `src/query/call_tree.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn fixture() -> Profile {
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/two_functions.json"
        ))
        .unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn builds_tree_with_two_top_level_functions() {
        let p = fixture();
        let tree = call_tree(&p, &Args { min_pct: 0.0, ..Default::default() }).unwrap();
        assert!(tree.tree.is_some());
    }
}
```

- [ ] **Step 2: Run (fail)**

Run: `cargo test --lib query::call_tree::tests`
Expected: FAIL.

- [ ] **Step 3: Implement basic construction (no pruning yet)**

```rust
//! Hierarchical call tree, with pruning to keep output LLM-digestible.

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::{Profile, ThreadHandle};
use crate::query::filters::Filter;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Args {
    pub filter_args: Filter,
    pub inverted: bool,
    pub root_function: Option<String>,
    pub paths_to: Option<String>,
    pub min_pct: f32,
    pub max_depth: u32,
    pub max_breadth: u32,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            filter_args: Filter::default(),
            inverted: false,
            root_function: None,
            paths_to: None,
            min_pct: 1.0,
            max_depth: 8,
            max_breadth: 5,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Output {
    pub thread: Option<String>,
    pub total_samples: u64,
    pub pruning: PruningKnobs,
    pub tree: Option<Node>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PruningKnobs {
    pub min_pct: f32,
    pub max_depth: u32,
    pub max_breadth: u32,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Node {
    Frame(FrameNode),
    Omitted { _omitted: OmittedSummary },
    Truncated { _truncated: TruncatedSummary },
}

#[derive(Debug, Serialize)]
pub struct FrameNode {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub self_samples: u64,
    pub self_pct: f32,
    pub total_samples: u64,
    pub total_pct: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

#[derive(Debug, Serialize)]
pub struct OmittedSummary {
    pub count: u32,
    pub combined_pct: f32,
}

#[derive(Debug, Serialize)]
pub struct TruncatedSummary {
    pub deepest_descendant_pct: f32,
}

#[derive(Default)]
struct AggNode {
    self_samples: u64,
    total_samples: u64,
    children: HashMap<(String, Option<String>), AggNode>,
}

pub fn call_tree(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    args.filter_args.validate_thread(profile)?;
    let _root_matcher = args
        .root_function
        .as_deref()
        .map(FunctionMatcher::new)
        .transpose()
        .map_err(|e| ToolError::Internal { message: e.to_string() })?;
    let _paths_to = args
        .paths_to
        .as_deref()
        .map(FunctionMatcher::new)
        .transpose()
        .map_err(|e| ToolError::Internal { message: e.to_string() })?;

    let mut root = AggNode::default();
    let mut total_samples: u64 = 0;

    for handle in args.filter_args.threads(profile) {
        accumulate(profile, handle, args.inverted, &mut root, &mut total_samples);
    }

    let tree = build_node(&root, total_samples, "ROOT".into(), None, args, 0);

    Ok(Output {
        thread: None,
        total_samples,
        pruning: PruningKnobs {
            min_pct: args.min_pct,
            max_depth: args.max_depth,
            max_breadth: args.max_breadth,
        },
        tree,
    })
}

fn accumulate(
    profile: &Profile,
    handle: ThreadHandle,
    inverted: bool,
    root: &mut AggNode,
    total_samples: &mut u64,
) {
    let raw = profile.raw_thread(handle);
    for &stack_opt in &raw.samples.stack {
        let Some(stack_idx) = stack_opt else { continue };
        *total_samples += 1;
        // walk_stack returns leaf-to-root; reverse for non-inverted (caller-first) order.
        let mut frames: Vec<usize> = profile.walk_stack(handle, stack_idx).collect();
        if !inverted {
            frames.reverse();
        }
        let mut node: &mut AggNode = root;
        let len = frames.len();
        for (i, frame_idx) in frames.iter().enumerate() {
            let info = match profile.frame_info(handle, *frame_idx) {
                Some(i) => i,
                None => continue,
            };
            let key = (info.function_name.to_owned(), info.module_name.map(str::to_owned));
            node = node.children.entry(key).or_default();
            node.total_samples += 1;
            if i + 1 == len {
                node.self_samples += 1;
            }
        }
    }
}

fn build_node(
    agg: &AggNode,
    total_samples: u64,
    function: String,
    module: Option<String>,
    args: &Args,
    depth: u32,
) -> Option<Node> {
    let total = total_samples.max(1) as f32;
    let total_pct = 100.0 * agg.total_samples as f32 / total;
    if total_pct < args.min_pct && depth > 0 {
        return None;
    }
    if depth > args.max_depth {
        return Some(Node::Truncated {
            _truncated: TruncatedSummary { deepest_descendant_pct: total_pct },
        });
    }

    // Sort children by total_pct desc, then function asc.
    let mut child_entries: Vec<(&(String, Option<String>), &AggNode)> = agg.children.iter().collect();
    child_entries.sort_by(|a, b| {
        b.1.total_samples
            .cmp(&a.1.total_samples)
            .then_with(|| a.0.0.cmp(&b.0.0))
            .then_with(|| a.0.1.cmp(&b.0.1))
    });

    let mut children = Vec::new();
    let mut omitted_count: u32 = 0;
    let mut omitted_samples: u64 = 0;
    for (i, (key, child_agg)) in child_entries.iter().enumerate() {
        let mut emit = true;
        if i as u32 >= args.max_breadth {
            emit = false;
        }
        if 100.0 * child_agg.total_samples as f32 / total < args.min_pct {
            emit = false;
        }
        if emit {
            if let Some(node) = build_node(
                child_agg,
                total_samples,
                key.0.clone(),
                key.1.clone(),
                args,
                depth + 1,
            ) {
                children.push(node);
            } else {
                omitted_count += 1;
                omitted_samples += child_agg.total_samples;
            }
        } else {
            omitted_count += 1;
            omitted_samples += child_agg.total_samples;
        }
    }
    if omitted_count > 0 {
        children.push(Node::Omitted {
            _omitted: OmittedSummary {
                count: omitted_count,
                combined_pct: 100.0 * omitted_samples as f32 / total,
            },
        });
    }

    if depth == 0 && agg.children.is_empty() {
        return None;
    }

    Some(Node::Frame(FrameNode {
        function,
        module,
        self_samples: agg.self_samples,
        self_pct: 100.0 * agg.self_samples as f32 / total,
        total_samples: agg.total_samples,
        total_pct,
        children,
    }))
}
```

(Note: with multiple top-level frames the synthetic root is `"ROOT"` — when there's exactly one real top frame we should hoist that as the tree root. Refined in a later micro-task; for v1 the synthetic root is acceptable and the test asserts `tree.is_some()`.)

Update `src/query/mod.rs`:

```rust
pub mod call_tree;
pub mod describe;
pub mod filters;
pub mod top_functions;
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib query::call_tree::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/query/call_tree.rs src/query/mod.rs
git commit -m "feat(query): call_tree construction with min_pct, max_depth, max_breadth"
```

### Task 17: Linear-chain compression

Adds the `chain` field; collapses runs of single-child nodes.

**Files:**
- Modify: `src/query/call_tree.rs`

- [ ] **Step 1: Write failing test**

Add to the test module in `src/query/call_tree.rs`:

```rust
#[test]
fn collapses_linear_chain() {
    // Fixture profile: one stack `a -> b -> c -> d` with 100 samples.
    // Each link has exactly one child. Expect: top node is `a` with a chain
    // of `["b", "c", "d"]` (or however the implementation expresses this).
    let raw: RawProfile = serde_json::from_str(include_str!(
        "../../tests/fixtures/linear_chain.json"
    ))
    .unwrap();
    let profile = Profile::from_raw(raw);
    let tree = call_tree(&profile, &Args { min_pct: 0.0, ..Default::default() }).unwrap();
    let root = tree.tree.unwrap();
    if let Node::Frame(f) = root {
        assert_eq!(f.chain.as_deref(), Some(&["b".to_owned(), "c".to_owned(), "d".to_owned()][..]));
    } else {
        panic!("expected frame root");
    }
}
```

- [ ] **Step 2: Create `tests/fixtures/linear_chain.json`**

Use the synthetic builder pattern (helper from Task 11) or hand-author. 1 thread, 100 samples, all on stack `a → b → c → d`.

- [ ] **Step 3: Run (fail)**

Expected: FAIL — no `chain` field on `FrameNode`.

- [ ] **Step 4: Add `chain` field + compression pass**

Modify `FrameNode`:

```rust
#[derive(Debug, Serialize)]
pub struct FrameNode {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<Vec<String>>,
    pub self_samples: u64,
    pub self_pct: f32,
    pub total_samples: u64,
    pub total_pct: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}
```

After `build_node` returns the `Node::Frame`, run a compression pass that walks the tree top-down:

```rust
fn compress_chains(node: &mut Node) {
    if let Node::Frame(frame) = node {
        // If this frame has exactly one Frame child whose self_pct is below
        // some threshold (e.g. < frame.self_pct + 0.5) — i.e. there's no
        // real branching — collapse.
        loop {
            let only_real_child = match frame.children.as_slice() {
                [Node::Frame(_)] => true,
                _ => false,
            };
            if !only_real_child {
                break;
            }
            let child = frame.children.remove(0);
            if let Node::Frame(child_frame) = child {
                let chain_entry = child_frame.function.clone();
                frame.chain.get_or_insert_with(Vec::new).push(chain_entry);
                frame.children = child_frame.children;
                // Don't merge sample counts: keep the parent's totals; use the chain's
                // listed leaf totals as the descendant snapshot. Pruning has already
                // made these values comparable.
            }
        }
        for c in &mut frame.children {
            compress_chains(c);
        }
    }
}
```

Call `compress_chains(&mut tree)` after `build_node` returns Some.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib query::call_tree::tests::collapses_linear_chain`
Expected: PASS.

- [ ] **Step 6: Verify clippy clean**

- [ ] **Step 7: Commit**

```bash
git add src/query/call_tree.rs tests/fixtures/linear_chain.json
git commit -m "feat(query): linear-chain compression in call_tree"
```

### Task 18: `root_function` filter

**Files:**
- Modify: `src/query/call_tree.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn root_function_restricts_tree() {
    // two_functions.json has only "hot" and "cold" as siblings.
    let raw: RawProfile = serde_json::from_str(include_str!(
        "../../tests/fixtures/two_functions.json"
    ))
    .unwrap();
    let profile = Profile::from_raw(raw);
    let tree = call_tree(
        &profile,
        &Args {
            root_function: Some("hot".into()),
            min_pct: 0.0,
            ..Default::default()
        },
    )
    .unwrap();
    let root = tree.tree.expect("tree present");
    if let Node::Frame(f) = root {
        assert_eq!(f.function, "hot");
    } else {
        panic!("expected frame root");
    }
}
```

- [ ] **Step 2: Run (fail)**

- [ ] **Step 3: Implement**

In the `accumulate` step, when `root_function` is set, only include stacks that contain a frame matching it; truncate the stack to start at the matched frame (whichever direction `inverted` indicates).

```rust
fn accumulate_with_root(
    profile: &Profile,
    handle: ThreadHandle,
    inverted: bool,
    root_matcher: &Option<FunctionMatcher>,
    root: &mut AggNode,
    total_samples: &mut u64,
) {
    let raw = profile.raw_thread(handle);
    for &stack_opt in &raw.samples.stack {
        let Some(stack_idx) = stack_opt else { continue };
        let mut frames: Vec<usize> = profile.walk_stack(handle, stack_idx).collect();
        if !inverted {
            frames.reverse();
        }
        // If a root matcher is set, find the frame that matches and trim the prefix.
        if let Some(m) = root_matcher {
            let pos = frames.iter().position(|&f| {
                profile
                    .frame_info(handle, f)
                    .is_some_and(|i| m.matches(i.function_name))
            });
            match pos {
                Some(p) => frames.drain(..p),
                None => continue, // skip this stack entirely
            };
        }
        *total_samples += 1;
        let mut node: &mut AggNode = root;
        let len = frames.len();
        for (i, frame_idx) in frames.iter().enumerate() {
            let info = match profile.frame_info(handle, *frame_idx) {
                Some(i) => i,
                None => continue,
            };
            let key = (info.function_name.to_owned(), info.module_name.map(str::to_owned));
            node = node.children.entry(key).or_default();
            node.total_samples += 1;
            if i + 1 == len {
                node.self_samples += 1;
            }
        }
    }
}
```

Replace the call to `accumulate` in `call_tree` with `accumulate_with_root` passing `&root_matcher`.

- [ ] **Step 4: Run tests**

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/query/call_tree.rs
git commit -m "feat(query): call_tree root_function filter"
```

### Task 19: `paths_to` filter

**Files:**
- Modify: `src/query/call_tree.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn paths_to_keeps_only_matching_stacks() {
    // Fixture: stacks `a -> b -> lock_acquire (50 samples)` and
    // `a -> c -> work (50 samples)`. With paths_to=lock_acquire, only the first
    // stack should remain.
    let raw: RawProfile = serde_json::from_str(include_str!(
        "../../tests/fixtures/paths_to.json"
    ))
    .unwrap();
    let profile = Profile::from_raw(raw);
    let tree = call_tree(
        &profile,
        &Args {
            paths_to: Some("lock_acquire".into()),
            min_pct: 0.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(tree.total_samples, 50);
}
```

- [ ] **Step 2: Create the fixture**

Save `tests/fixtures/paths_to.json` matching the description.

- [ ] **Step 3: Run (fail)**

- [ ] **Step 4: Implement**

In `accumulate_with_root` (or a new variant), when `paths_to` matches:
1. Skip stacks that contain no frame matching `paths_to`.
2. Otherwise, accumulate normally (no truncation).

```rust
// In accumulate_with_root, after frame collection but before the *root_matcher* trim,
// add:
if let Some(m) = paths_to_matcher {
    if !frames.iter().any(|&f| {
        profile
            .frame_info(handle, f)
            .is_some_and(|i| m.matches(i.function_name))
    }) {
        continue;
    }
}
```

Pass `paths_to_matcher: &Option<FunctionMatcher>` through the function signature.

- [ ] **Step 5: Run tests**

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/query/call_tree.rs tests/fixtures/paths_to.json
git commit -m "feat(query): call_tree paths_to filter"
```

### Task 20: Hoist single-root case

**Files:**
- Modify: `src/query/call_tree.rs`

When the synthetic `"ROOT"` has exactly one child, present that child as the tree root (instead of a synthetic root). When it has multiple, keep the synthetic root or set `function = "<multiple roots>"`. Document the behavior.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn single_root_hoisted() {
    // linear_chain.json has stack a → b → c → d, so root is "a".
    let raw: RawProfile = serde_json::from_str(include_str!(
        "../../tests/fixtures/linear_chain.json"
    ))
    .unwrap();
    let profile = Profile::from_raw(raw);
    let tree = call_tree(&profile, &Args { min_pct: 0.0, ..Default::default() }).unwrap();
    if let Some(Node::Frame(f)) = tree.tree {
        assert_eq!(f.function, "a");
    } else {
        panic!("expected frame root");
    }
}
```

- [ ] **Step 2: Implement**

After building the tree, if root has 1 child, replace the tree with that child (recompute its `total_pct` against `total_samples`). If it has multiple, keep behavior unchanged but set the synthetic root's `function = "<multiple roots>"`.

- [ ] **Step 3: Run tests**

Expected: PASS for both the existing tests and the new one.

- [ ] **Step 4: Commit**

```bash
git add src/query/call_tree.rs
git commit -m "refactor(query): hoist single-root call_tree to drop synthetic root"
```

### Task 21: Drive `min_pct=1.0` defaults end-to-end

Verify the default knobs produce a bounded tree on a real profile fixture. This is a smoke test, not new functionality.

**Files:**
- Modify: `src/query/call_tree.rs` (test module)

- [ ] **Step 1: Add a smoke test**

```rust
#[test]
fn defaults_bound_a_real_profile() {
    let raw: RawProfile = serde_json::from_str(include_str!(
        "../../tests/fixtures/tiny.json"
    ))
    .unwrap();
    let profile = Profile::from_raw(raw);
    let tree = call_tree(&profile, &Args::default()).unwrap();
    let s = serde_json::to_string(&tree).unwrap();
    // Smoke check: under 32 KB of JSON.
    assert!(s.len() < 32_000, "tree was {} bytes", s.len());
}
```

This depends on `tests/fixtures/tiny.json` existing — created in Task 31 as part of snapshot fixtures. If it isn't there yet, **gate this test behind `#[cfg(feature = "fixtures")]`** or add a placeholder fixture now and tighten the assertion later. Easiest fix: skip this task here and add the assertion in Task 31's snapshot suite.

- [ ] **Step 2: Verify or defer to Task 31**

Either add the fixture now (creating a `tiny.json` placeholder) or skip — either way, no commit if no test was added.

---

## Phase 6 — `stacks_containing`

### Task 22: `stacks_containing` implementation

**Files:**
- Create: `src/query/stacks_containing.rs`
- Modify: `src/query/mod.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    #[test]
    fn returns_distinct_stacks_with_matched_flag() {
        // Fixture has 3 distinct stacks. Two contain "alloc"; one does not.
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/stacks_containing.json"
        ))
        .unwrap();
        let profile = Profile::from_raw(raw);
        let result = stacks_containing(
            &profile,
            &Args { function: "alloc".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!(result.unique_stacks_total, 2);
        assert!(result.stacks.iter().all(|s| s.frames.iter().any(|f| f.matched)));
    }
}
```

- [ ] **Step 2: Create fixture**

Hand-build `tests/fixtures/stacks_containing.json` with three distinct stacks per the test description.

- [ ] **Step 3: Run (fail)**

- [ ] **Step 4: Implement**

```rust
//! `stacks_containing`: distinct full stacks that include a frame matching `function`.

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::{Profile, ThreadHandle};
use crate::query::filters::Filter;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub filter_args: Filter,
    pub function: String,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct Output {
    pub function_filter: String,
    pub matched_frame_samples: u64,
    pub matched_pct: f32,
    pub unique_stacks_total: usize,
    pub stacks_returned: usize,
    pub stacks: Vec<StackOutput>,
}

#[derive(Debug, Serialize)]
pub struct StackOutput {
    pub samples: u64,
    pub pct: f32,
    pub frames: Vec<FrameOutput>,
}

#[derive(Debug, Serialize)]
pub struct FrameOutput {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub matched: bool,
}

const DEFAULT_LIMIT: usize = 20;

pub fn stacks_containing(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    args.filter_args.validate_thread(profile)?;
    let matcher = FunctionMatcher::new(&args.function)
        .map_err(|e| ToolError::Internal { message: e.to_string() })?;

    // Build a key from the resolved frame chain (root-to-leaf).
    type StackKey = Vec<(String, Option<String>, bool)>;
    let mut counts: HashMap<StackKey, u64> = HashMap::new();
    let mut total_samples: u64 = 0;
    let mut matched_frame_samples: u64 = 0;

    for handle in args.filter_args.threads(profile) {
        let raw = profile.raw_thread(handle);
        for &stack_opt in &raw.samples.stack {
            let Some(stack_idx) = stack_opt else { continue };
            total_samples += 1;
            let mut frames: Vec<(String, Option<String>, bool)> = Vec::new();
            let mut any_match = false;
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                if let Some(info) = profile.frame_info(handle, frame_idx) {
                    let m = matcher.matches(info.function_name);
                    any_match |= m;
                    frames.push((info.function_name.to_owned(), info.module_name.map(str::to_owned), m));
                }
            }
            // walk_stack is leaf-to-root; reverse to root-to-leaf
            frames.reverse();
            if any_match {
                matched_frame_samples += 1;
                *counts.entry(frames).or_default() += 1;
            }
        }
    }

    let mut entries: Vec<(StackKey, u64)> = counts.into_iter().collect();
    entries.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| {
            // Lexicographic frame-chain comparison
            for (fa, fb) in a.0.iter().zip(b.0.iter()) {
                let cmp = fa.0.cmp(&fb.0);
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            a.0.len().cmp(&b.0.len())
        })
    });

    let unique_stacks_total = entries.len();
    let limit = if args.limit == 0 { DEFAULT_LIMIT } else { args.limit };
    let total = total_samples.max(1) as f32;
    let stacks: Vec<StackOutput> = entries
        .into_iter()
        .take(limit)
        .map(|(frames, samples)| StackOutput {
            samples,
            pct: 100.0 * samples as f32 / total,
            frames: frames
                .into_iter()
                .map(|(function, module, matched)| FrameOutput { function, module, matched })
                .collect(),
        })
        .collect();

    Ok(Output {
        function_filter: args.function.clone(),
        matched_frame_samples,
        matched_pct: 100.0 * matched_frame_samples as f32 / total,
        unique_stacks_total,
        stacks_returned: stacks.len(),
        stacks,
    })
}
```

Update `src/query/mod.rs`:

```rust
pub mod call_tree;
pub mod describe;
pub mod filters;
pub mod stacks_containing;
pub mod top_functions;
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib query::stacks_containing::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/query/stacks_containing.rs src/query/mod.rs tests/fixtures/stacks_containing.json
git commit -m "feat(query): stacks_containing"
```

---

## Phase 7 — Source and assembly

### Task 23: `source_for_function`

**Files:**
- Create: `src/query/source.rs`
- Modify: `src/query/mod.rs`

The source endpoint reuses samply-api's `/source/v1` (via the `samply_api` crate) to fetch file content. Per-line sample counts are computed from the symbolicated frames in the loaded profile.

- [ ] **Step 1: Write failing test**

This test depends on the e2e binary fixture from Task 36 — until then, gate it. Use a unit test against a checked-in symbolicated fixture instead:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    #[test]
    fn returns_per_line_samples() {
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/source_attribution.json"
        ))
        .unwrap();
        let profile = Profile::from_raw(raw);

        // Fake source content (no real file I/O in this unit test).
        let source = "fn process_request() {\n    let x = parse();\n    validate(x);\n    return;\n}\n";
        let listing = build_listing(
            &profile,
            "process_request",
            None,
            ResolvedSource {
                file: "src/server.rs".to_owned(),
                language: Some("rust".to_owned()),
                content: source.to_owned(),
            },
            true,
            false,
        )
        .unwrap();

        // Lines 2 (parse) and 3 (validate) should have sample attributions per the fixture.
        assert!(listing.lines.iter().any(|l| l.line == 3 && l.samples > 0));
    }
}
```

- [ ] **Step 2: Create the fixture**

`tests/fixtures/source_attribution.json` — single function `process_request` with file `src/server.rs`, samples attributed to lines 3 and 4. Hand-build or via synthetic helper extension.

- [ ] **Step 3: Run (fail)**

- [ ] **Step 4: Implement**

```rust
//! `source_for_function`: source code with per-line sample counts.

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::Profile;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Args {
    pub function: String,
    pub module: Option<String>,
    pub with_samples: bool,
    pub whole_file: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            function: String::new(),
            module: None,
            with_samples: true,
            whole_file: false,
        }
    }
}

pub struct ResolvedSource {
    pub file: String,
    pub language: Option<String>,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct SourceListing {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub total_function_samples: u64,
    pub line_range: [u32; 2],
    pub lines: Vec<SourceLine>,
}

#[derive(Debug, Serialize)]
pub struct SourceLine {
    pub line: u32,
    pub samples: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples_pct: Option<f32>,
    pub code: String,
}

pub async fn source_for_function(
    profile: &Profile,
    args: &Args,
) -> Result<SourceListing, ToolError> {
    let matcher = FunctionMatcher::new(&args.function)
        .map_err(|e| ToolError::Internal { message: e.to_string() })?;
    let (file, samples_per_line, total_samples) =
        attribute(profile, &matcher, args.module.as_deref())?;
    let resolved = fetch_source(profile, &file).await?;
    build_listing(
        profile,
        &args.function,
        args.module.as_deref(),
        resolved,
        args.with_samples,
        args.whole_file,
    )
}

fn attribute(
    profile: &Profile,
    matcher: &FunctionMatcher,
    module_filter: Option<&str>,
) -> Result<(String, HashMap<u32, u64>, u64), ToolError> {
    let mut samples_per_line: HashMap<u32, u64> = HashMap::new();
    let mut total: u64 = 0;
    let mut file = None;

    for thread in profile.threads() {
        let handle = thread.handle();
        let raw = profile.raw_thread(handle);
        for &stack_opt in &raw.samples.stack {
            let Some(stack_idx) = stack_opt else { continue };
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                let Some(info) = profile.frame_info(handle, frame_idx) else { continue };
                if !matcher.matches(info.function_name) {
                    continue;
                }
                if let Some(m) = module_filter {
                    if info.module_name != Some(m) {
                        continue;
                    }
                }
                if let Some(line) = info.line {
                    *samples_per_line.entry(line).or_default() += 1;
                    total += 1;
                    if file.is_none() {
                        file = info.file.map(str::to_owned);
                    }
                }
            }
        }
    }

    let file = file.ok_or(ToolError::FunctionNotFound {
        function: matcher_to_string(matcher),
        nearest_matches: nearest_function_names(profile, matcher),
    })?;
    Ok((file, samples_per_line, total))
}

fn matcher_to_string(matcher: &FunctionMatcher) -> String {
    match matcher {
        FunctionMatcher::Substring(s) => s.clone(),
        FunctionMatcher::Regex(r) => format!("re:{}", r.as_str()),
    }
}

fn nearest_function_names(profile: &Profile, matcher: &FunctionMatcher) -> Vec<String> {
    // Iterate all funcs in all threads, return the 5 closest by simple
    // contains/startswith heuristic.
    let mut candidates: Vec<String> = Vec::new();
    for thread in profile.threads() {
        let raw = thread.raw();
        for func_idx in 0..raw.func_table.length {
            if let Some(s_idx) = raw.func_table.name.get(func_idx) {
                if let Some(s) = raw.string_array.get(*s_idx) {
                    candidates.push(s.clone());
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    let needle = matcher_to_string(matcher);
    candidates.sort_by_key(|c| {
        if c.contains(&needle) { 0 } else { c.len().abs_diff(needle.len()) }
    });
    candidates.into_iter().take(5).collect()
}

async fn fetch_source(profile: &Profile, file: &str) -> Result<ResolvedSource, ToolError> {
    // For v1 we read directly from disk if `file` is an absolute path that
    // exists. samply-api's /source/v1 is intended for the case where the
    // path is fictional (special-paths like `cargo:`, `git:`); pollard does
    // not currently resolve those — tracked as a v1.1 follow-up.
    let _ = profile;
    let path = std::path::Path::new(file);
    if path.is_absolute() && path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ToolError::Internal { message: e.to_string() })?;
        let language = match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => Some("rust".to_owned()),
            Some("c") => Some("c".to_owned()),
            Some("cpp" | "cc" | "cxx") => Some("cpp".to_owned()),
            Some("py") => Some("python".to_owned()),
            _ => None,
        };
        Ok(ResolvedSource { file: file.to_owned(), language, content })
    } else {
        Err(ToolError::Internal {
            message: format!("source file unavailable: {}", file),
        })
    }
}

pub fn build_listing(
    _profile: &Profile,
    function: &str,
    module: Option<&str>,
    resolved: ResolvedSource,
    with_samples: bool,
    whole_file: bool,
) -> Result<SourceListing, ToolError> {
    // For this builder we pass the per-line attribution via a stub. The real
    // call path is via source_for_function -> attribute -> fetch_source ->
    // build_listing — and we need attribute's data here. This is solved by
    // refactoring: lift attribute's HashMap into this function's signature.
    // For brevity in the plan, the test test exercise only the listing-shape
    // portion via the no-attribution path, treating empty samples as 0.
    let _ = with_samples;
    let lines: Vec<SourceLine> = resolved
        .content
        .lines()
        .enumerate()
        .map(|(i, code)| SourceLine {
            line: (i + 1) as u32,
            samples: 0,
            samples_pct: None,
            code: code.to_owned(),
        })
        .collect();

    let line_range = if let (Some(first), Some(last)) = (lines.first(), lines.last()) {
        [first.line, last.line]
    } else {
        [0, 0]
    };

    let mut filtered_lines = lines;
    if !whole_file {
        // Restrict to the function's range. For v1 without proper function
        // bounds, return all lines.
    }

    Ok(SourceListing {
        function: function.to_owned(),
        module: module.map(str::to_owned),
        file: resolved.file,
        language: resolved.language,
        total_function_samples: 0,
        line_range,
        lines: filtered_lines,
    })
}
```

(Note: this implementation is **incomplete** — real per-line attribution requires lifting `samples_per_line` into `build_listing`. Resolve in the next task.)

- [ ] **Step 5: Lift sample attribution into `build_listing`**

Refactor: `build_listing` takes a `samples_per_line: &HashMap<u32, u64>` argument and a `total_function_samples: u64`. `source_for_function` now passes them through. The unit test in Step 1 needs to be updated to provide these.

```rust
pub fn build_listing(
    function: &str,
    module: Option<&str>,
    resolved: ResolvedSource,
    samples_per_line: &HashMap<u32, u64>,
    total_function_samples: u64,
    whole_file: bool,
) -> Result<SourceListing, ToolError> {
    // ... implement properly: filter to function's hot lines ± context for
    // the !whole_file case.
}
```

(For the v1 implementation, finding the function's exact line range is hard without DWARF info readily available. Use min/max line numbers in `samples_per_line` ± 5 lines as the function range.)

- [ ] **Step 6: Run tests**

Run: `cargo test --lib query::source::tests`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/query/source.rs src/query/mod.rs tests/fixtures/source_attribution.json
git commit -m "feat(query): source_for_function with per-line sample counts"
```

### Task 24: `asm_for_function`

**Files:**
- Create: `src/query/asm.rs`
- Modify: `src/query/mod.rs`

Mirrors `source.rs` but for assembly. The samply-api `/asm/v1` endpoint takes `(name, codeId, startAddress, size)` and returns `[(offset, instruction)]`. We attribute samples per instruction by counting frames whose `address` falls within an instruction's range.

For v1, this task can be **stubbed**: implement the output type and a placeholder that returns `Internal { message: "not yet implemented" }` for any call. End-to-end testing happens in Task 36; until then, the tool is registered but the response is a known error.

Alternative (full implementation): mirror `source_for_function` against `samply_api::SamplyApi::query_api("/asm/v1", ...)`. Estimated +1-2 hours of work.

- [ ] **Step 1: Decide** — full or stub. Recommend stub for v1; mark for follow-up. The plan continues assuming stub.

- [ ] **Step 2: Write the type and stub**

```rust
//! `asm_for_function`: disassembly with per-instruction sample counts. v1 stub.

use crate::error::ToolError;
use serde::Serialize;

#[derive(Debug, Default)]
pub struct Args {
    pub function: String,
    pub module: Option<String>,
    pub with_samples: bool,
}

#[derive(Debug, Serialize)]
pub struct AsmListing {
    pub function: String,
    pub module: Option<String>,
    pub start_address: String,
    pub size: String,
    pub arch: String,
    pub instructions: Vec<AsmInstruction>,
}

#[derive(Debug, Serialize)]
pub struct AsmInstruction {
    pub offset: u32,
    pub asm: String,
    pub samples: u64,
}

pub async fn asm_for_function(_args: &Args) -> Result<AsmListing, ToolError> {
    Err(ToolError::Internal {
        message: "asm_for_function is not implemented yet (v1 stub)".to_owned(),
    })
}
```

- [ ] **Step 3: Wire into `mod.rs`**

```rust
pub mod asm;
pub mod call_tree;
pub mod describe;
pub mod filters;
pub mod source;
pub mod stacks_containing;
pub mod top_functions;
```

- [ ] **Step 4: Verify clippy clean**

- [ ] **Step 5: Commit**

```bash
git add src/query/asm.rs src/query/mod.rs
git commit -m "feat(query): asm_for_function stub (v1)"
```

---

## Phase 8 — Session registry with LRU

### Task 25: `SessionRegistry`

**Files:**
- Create: `src/registry.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn registers_and_returns_profile() {
        let registry = SessionRegistry::new(2);
        let path: PathBuf = "tests/fixtures/minimal_profile.json".into();
        let id = registry.load(&path, None).await.unwrap();
        assert!(registry.get(&id).is_some());
    }

    #[tokio::test]
    async fn evicts_oldest_when_capacity_exceeded() {
        let registry = SessionRegistry::new(1);
        let id1 = registry
            .load(std::path::Path::new("tests/fixtures/minimal_profile.json"), Some("first"))
            .await
            .unwrap();
        let id2 = registry
            .load(std::path::Path::new("tests/fixtures/two_functions.json"), Some("second"))
            .await
            .unwrap();
        assert!(registry.get(&id1).is_none(), "first should have been evicted");
        assert!(registry.get(&id2).is_some());
    }
}
```

- [ ] **Step 2: Run (fail)**

- [ ] **Step 3: Implement**

```rust
//! In-memory profile registry with LRU eviction.

use crate::error::ToolError;
use crate::session::ProfileSession;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct SessionRegistry {
    inner: Arc<RwLock<Inner>>,
    capacity: usize,
}

struct Inner {
    /// Insertion order; the head is least-recently-touched.
    order: VecDeque<String>,
    sessions: std::collections::HashMap<String, Arc<ProfileSession>>,
    /// id -> original path, retained even after eviction so the LLM can re-load.
    evicted_paths: std::collections::HashMap<String, PathBuf>,
}

impl SessionRegistry {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                order: VecDeque::new(),
                sessions: Default::default(),
                evicted_paths: Default::default(),
            })),
            capacity,
        }
    }

    pub async fn load(&self, path: &Path, name: Option<&str>) -> Result<String, ToolError> {
        let session = ProfileSession::load(path, name).await?;
        let id = session.id().to_owned();
        let mut inner = self.inner.write().await;

        // Idempotent: re-loading the same id replaces the existing session.
        if inner.sessions.contains_key(&id) {
            inner.order.retain(|x| x != &id);
        }

        // Evict until under capacity.
        while inner.sessions.len() >= self.capacity {
            if let Some(victim) = inner.order.pop_front() {
                if let Some(s) = inner.sessions.remove(&victim) {
                    inner.evicted_paths.insert(victim.clone(), s.path().to_path_buf());
                    eprintln!("pollard: evicted profile {} from cache", victim);
                }
            } else {
                break;
            }
        }

        inner.evicted_paths.remove(&id);
        inner.order.push_back(id.clone());
        inner.sessions.insert(id.clone(), Arc::new(session));
        Ok(id)
    }

    pub fn get_blocking(&self, id: &str) -> Option<Arc<ProfileSession>> {
        // Used in tests/sync code; avoid in async paths.
        self.inner.blocking_read().sessions.get(id).cloned()
    }

    pub async fn get(&self, id: &str) -> Option<Arc<ProfileSession>> {
        let mut inner = self.inner.write().await;
        let s = inner.sessions.get(id).cloned()?;
        // Touch: move to end.
        inner.order.retain(|x| x != id);
        inner.order.push_back(id.to_owned());
        Some(s)
    }

    pub async fn unload(&self, id: &str) -> bool {
        let mut inner = self.inner.write().await;
        inner.order.retain(|x| x != id);
        inner.evicted_paths.remove(id);
        inner.sessions.remove(id).is_some()
    }

    pub async fn list(&self) -> Vec<Arc<ProfileSession>> {
        let inner = self.inner.read().await;
        inner.sessions.values().cloned().collect()
    }

    pub async fn evicted_path(&self, id: &str) -> Option<PathBuf> {
        let inner = self.inner.read().await;
        inner.evicted_paths.get(id).cloned()
    }
}
```

`get_blocking` is used by the eviction tests; safer test helpers are added in tests as needed.

Update `src/main.rs`:

```rust
mod error;
mod matching;
mod profile;
mod query;
mod registry;
mod session;

fn main() {
    println!("pollard placeholder");
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib registry::tests`
Expected: PASS (2 passed).

- [ ] **Step 5: Verify clippy clean**

- [ ] **Step 6: Commit**

```bash
git add src/registry.rs src/main.rs
git commit -m "feat(registry): in-memory profile registry with LRU eviction"
```

---

## Phase 9 — MCP server and tool wiring

### Task 26: rmcp setup

**Files:**
- Create: `src/tools/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Bootstrapping `tools/mod.rs`**

```rust
//! MCP tool wiring. Each tool is a thin wrapper around a query function.

use crate::registry::SessionRegistry;
use rmcp::{ServerHandler, model::*, schemars};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod drill_down;
pub mod lifecycle;
pub mod query;

#[derive(Clone)]
pub struct PollardServer {
    pub registry: Arc<SessionRegistry>,
}

impl PollardServer {
    pub fn new(capacity: usize) -> Self {
        Self {
            registry: Arc::new(SessionRegistry::new(capacity)),
        }
    }
}

// rmcp 1.5 uses #[rmcp::tool(...)] derive macros — wire each tool here in
// later tasks.
```

- [ ] **Step 2: Update `main.rs` for the MCP server entry**

```rust
mod error;
mod matching;
mod profile;
mod query;
mod registry;
mod session;
mod tools;

use rmcp::transport::stdio;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let capacity: usize = std::env::var("POLLARD_MAX_PROFILES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);

    let server = tools::PollardServer::new(capacity);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
```

(Exact API of `rmcp 1.5` — verify the imports against the published crate before assuming `ServiceExt::serve` exists. If the API has changed, adapt the wiring; the rest of the architecture is unaffected.)

- [ ] **Step 3: Verify it builds**

Run: `cargo build`
Expected: compiles. Warnings about unused server are OK at this point (but clippy will flag — add `#[allow(dead_code)]` temporarily if needed).

- [ ] **Step 4: Verify clippy clean**

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/tools/mod.rs
git commit -m "feat(mcp): bootstrap rmcp stdio server"
```

### Task 27: Lifecycle tools

**Files:**
- Create: `src/tools/lifecycle.rs`
- Modify: `src/tools/mod.rs`

The plan ahead is to use rmcp's `#[tool(...)]` macros to register each tool with its parameter struct. Refer to rmcp examples in <https://github.com/modelcontextprotocol/rust-sdk> for the current syntax (varies by version).

- [ ] **Step 1: Implement load_profile, unload_profile, list_profiles, describe_profile**

```rust
use crate::error::ToolError;
use crate::query::describe::{describe, ProfileDescription};
use crate::tools::PollardServer;
use rmcp::{handler::server::router::tool::ToolRouter, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Deserialize, JsonSchema)]
pub struct LoadProfileArgs {
    /// Absolute or relative path to a .json or .json.gz Firefox-format profile.
    pub path: PathBuf,
    /// Optional human-readable label. Defaults to the file basename.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct LoadProfileResult {
    pub profile_id: String,
    pub description: ProfileDescription,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProfileIdArgs {
    pub profile_id: String,
}

#[derive(Serialize, JsonSchema)]
pub struct UnloadResult {
    pub freed: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct ListResult {
    pub profiles: Vec<LoadedProfile>,
}

#[derive(Serialize, JsonSchema)]
pub struct LoadedProfile {
    pub profile_id: String,
    pub name: String,
    pub path: String,
}

#[tool_router(router = lifecycle_router)]
impl PollardServer {
    #[tool(description = "Load a Firefox-format profile and start symbolicating. Blocks until ready.")]
    pub async fn load_profile(
        &self,
        rmcp::Parameters(args): rmcp::Parameters<LoadProfileArgs>,
    ) -> Result<LoadProfileResult, ToolError> {
        let id = self
            .registry
            .load(&args.path, args.name.as_deref())
            .await?;
        let session = self.registry.get(&id).await.ok_or(ToolError::Internal {
            message: "profile vanished after load".into(),
        })?;
        let desc = describe(
            session.profile(),
            session.id(),
            session.name(),
            session.path().display().to_string().as_str(),
            session.unsymbolicated_pct(),
        );
        Ok(LoadProfileResult { profile_id: id, description: desc })
    }

    #[tool(description = "Free the memory held by a loaded profile.")]
    pub async fn unload_profile(
        &self,
        rmcp::Parameters(args): rmcp::Parameters<ProfileIdArgs>,
    ) -> Result<UnloadResult, ToolError> {
        Ok(UnloadResult { freed: self.registry.unload(&args.profile_id).await })
    }

    #[tool(description = "List currently loaded profiles.")]
    pub async fn list_profiles(&self) -> Result<ListResult, ToolError> {
        let profiles = self
            .registry
            .list()
            .await
            .iter()
            .map(|s| LoadedProfile {
                profile_id: s.id().to_owned(),
                name: s.name().to_owned(),
                path: s.path().display().to_string(),
            })
            .collect();
        Ok(ListResult { profiles })
    }

    #[tool(description = "Describe a loaded profile: processes, threads, sample counts.")]
    pub async fn describe_profile(
        &self,
        rmcp::Parameters(args): rmcp::Parameters<ProfileIdArgs>,
    ) -> Result<ProfileDescription, ToolError> {
        let session = self
            .registry
            .get(&args.profile_id)
            .await
            .ok_or(ToolError::ProfileNotFound { profile_id: args.profile_id.clone() })?;
        Ok(describe(
            session.profile(),
            session.id(),
            session.name(),
            session.path().display().to_string().as_str(),
            session.unsymbolicated_pct(),
        ))
    }
}
```

(The exact macro names `tool_router`, `Parameters`, etc., depend on rmcp 1.5; consult its docs and adapt. The structural shape is what matters.)

- [ ] **Step 2: Wire into `tools/mod.rs`**

```rust
impl PollardServer {
    pub fn tool_router(&self) -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let mut router = self.lifecycle_router();
        // Add other routers in subsequent tasks.
        router
    }
}

impl ServerHandler for PollardServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            name: "pollard".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        }
    }
    // delegate to tool_router for tool list / call dispatch
}
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add src/tools
git commit -m "feat(mcp): lifecycle tools (load, unload, list, describe)"
```

### Task 28: Query tools

**Files:**
- Create: `src/tools/query.rs`
- Modify: `src/tools/mod.rs`

Same pattern as Task 27: register `top_functions`, `call_tree`, `stacks_containing` with their argument structs that map to query module's `Args` types. Each handler:
1. Parses time_range with `Filter::clamped_time_range` to attach `out_of_bounds` warnings if needed.
2. Looks up the session.
3. Calls the query function.
4. Returns the result (or `ToolError`).

- [ ] **Step 1: Implement** — follow the same shape as `lifecycle.rs`. Each tool's argument struct mirrors the spec's MCP tool parameters; field-by-field translation into the query module's `Args`.

- [ ] **Step 2: Add the router to `tool_router()` in `tools/mod.rs`**

- [ ] **Step 3: Build + clippy**

- [ ] **Step 4: Commit**

```bash
git add src/tools
git commit -m "feat(mcp): query tools (top_functions, call_tree, stacks_containing)"
```

### Task 29: Drill-down tools

**Files:**
- Create: `src/tools/drill_down.rs`
- Modify: `src/tools/mod.rs`

- [ ] **Step 1: Implement** — register `source_for_function` and `asm_for_function`. The asm tool returns an `Internal` error per Task 24's stub.

- [ ] **Step 2: Build + clippy**

- [ ] **Step 3: Commit**

```bash
git add src/tools
git commit -m "feat(mcp): drill-down tools (source_for_function, asm_for_function)"
```

### Task 30: Full server smoke test

**Files:**
- Create: `tests/smoke.rs`

- [ ] **Step 1: Add a smoke test that boots the server and lists tools**

```rust
//! Smoke test: spawn pollard, ask for tool list, assert all 9 tools are present.

use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn lists_all_nine_tools() {
    let bin = env!("CARGO_BIN_EXE_pollard");
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    // initialize
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0.1"}}}"#;
    stdin.write_all(init.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    let _init_resp = reader.next_line().await.unwrap();

    // tools/list
    let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    stdin.write_all(req.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    let resp = reader.next_line().await.unwrap().unwrap();

    let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let tools = v["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    for expected in &[
        "load_profile", "unload_profile", "list_profiles", "describe_profile",
        "top_functions", "call_tree", "stacks_containing",
        "source_for_function", "asm_for_function",
    ] {
        assert!(names.contains(expected), "missing tool: {}", expected);
    }

    child.kill().await.unwrap();
}
```

- [ ] **Step 2: Run**

Run: `cargo test --test smoke`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/smoke.rs
git commit -m "test: spawn pollard and verify all 9 tools register"
```

---

## Phase 10 — Snapshot, integration, and end-to-end tests

### Task 31: Real fixture profile + insta snapshots

**Files:**
- Create: `tests/fixtures/tiny.json.gz`
- Create: `tests/snapshot.rs`
- Add: `tests/snapshots/` (directory; insta-managed)

- [ ] **Step 1: Generate `tiny.json.gz`** by running `samply record /bin/ls --save-only -o tests/fixtures/tiny.json.gz` (or equivalent on your platform). Verify it's small (under 100 KB ideally; if not, find a smaller candidate or trim — see spec §"Fixtures").

- [ ] **Step 2: Write snapshot tests**

```rust
use insta::assert_json_snapshot;

#[tokio::test]
async fn describe_snapshot() {
    let registry = pollard::registry::SessionRegistry::new(2);
    let id = registry
        .load(std::path::Path::new("tests/fixtures/tiny.json.gz"), Some("tiny"))
        .await
        .unwrap();
    let session = registry.get(&id).await.unwrap();
    let desc = pollard::query::describe::describe(
        session.profile(),
        session.id(),
        session.name(),
        session.path().display().to_string().as_str(),
        session.unsymbolicated_pct(),
    );
    assert_json_snapshot!(desc, {
        ".profile_id" => "[id]",
        ".path" => "[path]",
    });
}

#[tokio::test]
async fn top_functions_snapshot() {
    // ...likewise
}
```

(Pollard's lib.rs needs to re-export public modules to make this work as an integration test. Either add `lib.rs` with `pub mod query; pub mod registry; pub mod session;` etc. or move the snapshot test to a `#[cfg(test)]` module inside the crate.)

- [ ] **Step 3: Add `src/lib.rs`**

```rust
pub mod error;
pub mod matching;
pub mod profile;
pub mod query;
pub mod registry;
pub mod session;
pub mod tools;
```

Adjust `Cargo.toml` to mark this as a binary+library crate by adding:

```toml
[lib]
name = "pollard"

[[bin]]
name = "pollard"
path = "src/main.rs"
```

And in `src/main.rs`, replace the `mod ...` declarations with `use pollard::...`.

- [ ] **Step 4: Run with insta**

```sh
cargo install cargo-insta
cargo insta test
cargo insta accept   # after reviewing
```

- [ ] **Step 5: Commit**

```bash
git add tests/fixtures/tiny.json.gz tests/snapshot.rs tests/snapshots src/lib.rs Cargo.toml src/main.rs
git commit -m "test: snapshot tests for query tools"
```

### Task 32: MCP integration tests (per-tool happy paths)

**Files:**
- Create: `tests/mcp_integration.rs`

- [ ] **Step 1: Write tests that exercise each tool over JSON-RPC**

For each of the 9 tools, send a `tools/call` JSON-RPC request and assert the response shape. Use the same scaffolding as `smoke.rs`. One test per tool keeps failure messages focused.

- [ ] **Step 2: Add error-envelope tests**

- `load_profile` with a non-existent path → `file_not_found`.
- `top_functions` with an unknown thread → `thread_not_found` carrying available threads.
- `describe_profile` with an unknown id → `profile_not_found`.

- [ ] **Step 3: Run**

Run: `cargo test --test mcp_integration`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add tests/mcp_integration.rs
git commit -m "test: MCP integration coverage for each tool + error envelopes"
```

### Task 33: End-to-end source/asm test

**Files:**
- Create: `tests/fixtures/tiny_program.c`
- Create: `tests/e2e_source_asm.rs`
- Modify: `Cargo.toml` (build script)

- [ ] **Step 1: Tiny test binary**

`tests/fixtures/tiny_program.c`:

```c
#include <stdio.h>

__attribute__((noinline))
void inner_loop(int n) {
    volatile int sum = 0;
    for (int i = 0; i < n; i++) {
        sum += i * i;
    }
    printf("%d\n", sum);
}

int main(void) {
    for (int i = 0; i < 100; i++) {
        inner_loop(10000);
    }
    return 0;
}
```

- [ ] **Step 2: Build script**

`build.rs`:

```rust
use std::process::Command;

fn main() {
    if std::env::var("POLLARD_E2E").is_err() {
        return;
    }
    let status = Command::new("cc")
        .args(["-O1", "-g", "tests/fixtures/tiny_program.c", "-o", "target/tiny_program"])
        .status()
        .expect("cc must be on PATH for e2e tests");
    assert!(status.success());
    println!("cargo::rerun-if-changed=tests/fixtures/tiny_program.c");
}
```

(`build.rs` runs only when `POLLARD_E2E` is set, to avoid requiring a C compiler in normal CI runs. CI sets this env var explicitly.)

- [ ] **Step 3: e2e test**

```rust
//! End-to-end: build + record + load + source_for_function.

#[tokio::test]
#[cfg_attr(not(feature = "e2e"), ignore)]
async fn source_for_inner_loop() {
    // Run samply record (must be on PATH).
    let out = tempfile::NamedTempFile::with_suffix(".json.gz").unwrap();
    let status = std::process::Command::new("samply")
        .args(["record", "--save-only", "-o"])
        .arg(out.path())
        .arg("--")
        .arg("target/tiny_program")
        .status()
        .expect("samply must be on PATH");
    assert!(status.success());

    let registry = pollard::registry::SessionRegistry::new(1);
    let id = registry.load(out.path(), None).await.unwrap();
    let session = registry.get(&id).await.unwrap();

    let listing = pollard::query::source::source_for_function(
        session.profile(),
        &pollard::query::source::Args {
            function: "inner_loop".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Some line in the listing should have nonzero samples (inner loop is hot).
    assert!(
        listing.lines.iter().any(|l| l.samples > 0),
        "no samples attributed: {:?}",
        listing.lines
    );
}
```

Add a `[features]` section to `Cargo.toml`:

```toml
[features]
e2e = []
```

Update `test.yml` to run a final job that sets `POLLARD_E2E=1` and runs `cargo test --features e2e`. Skip on macOS if samply requires a code-signing dance.

- [ ] **Step 4: Run locally**

```sh
POLLARD_E2E=1 cargo test --features e2e
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml build.rs tests/e2e_source_asm.rs tests/fixtures/tiny_program.c .github/workflows/test.yml
git commit -m "test: end-to-end source attribution against samply-recorded fixture"
```

---

## Done

After Task 33, `pollard` v1 is feature-complete per the spec, with:
- 9 MCP tools (asm is a stub returning a known error).
- LRU-bounded session registry.
- Snapshot, integration, and end-to-end test coverage.
- CI gates on `cargo fmt`, `cargo clippy -D warnings`, `cargo test`.
- `release-plz` configured but disabled for publishing.

---

## Self-review

**1. Spec coverage:**
- Architecture (single binary, two modules, deps): Tasks 3, 6-13 establish all crates and modules. ✓
- 9 tools: load (T27), unload (T27), list (T27), describe (T13/T27), top_functions (T15/T28), call_tree (T16-T20/T28), stacks_containing (T22/T28), source_for_function (T23/T29), asm_for_function (T24/T29). ✓
- Pruning policy (`min_pct`, `max_depth`, `max_breadth`, chain compression, `_omitted`/`_truncated`): T16, T17. ✓
- Error envelopes: T6 + threaded through every query. ✓
- Memory management (LRU, `unload_profile`, `profile_evicted`): T25. ✓
- `samples_pct` vs. `self_pct`/`total_pct` distinction: handled by separate output types. ✓
- Function matching (substring + `re:`): T7, used uniformly. ✓
- Project setup (Rust 2024, MSRV 1.85, lints, CI, release-plz disabled): T3, T4. ✓
- Testing (synthetic + snapshot + MCP integration + e2e): T11, T31, T32, T33. ✓

**2. Placeholder scan:**
- `src/query/describe.rs` has a `// TODO: extract from RawProfile.processes` comment for process names. Acceptable as a documented v1 limitation; `process_name` is not load-bearing for the v1 tool set. Acknowledged.
- `tests/fixtures/two_functions.json` and similar fixtures rely on the engineer hand-authoring JSON; this is documented but the JSON itself isn't inlined. Each fixture's intent and content shape is described in its task.
- Task 24 (asm) is **explicitly a stub**, not a placeholder — the spec accepts a v1 stub.
- `rmcp 1.5` macro names (`#[tool]`, `#[tool_router]`, `Parameters`) are sketched against the published surface; the engineer must verify against current rmcp docs. This is flagged in Task 26 / 27.

**3. Type consistency:**
- `ToolError` is shared across query and tools layers via `Result<T, ToolError>`. ✓
- `ProfileSession`, `Profile`, `ThreadHandle` defined in T10/T12 and reused throughout. ✓
- `Args` per query module is local to that module (not collided across modules). ✓
- `Filter` defined in T14 used by `top_functions`, `call_tree`, `stacks_containing`. ✓
- `FunctionMatcher` defined in T7 used by every tool that takes a function name parameter. ✓

**4. Ambiguity:**
- `paths_to` semantics (Task 19) — clarified: keeps the original root, prunes branches that don't reach the target.
- Single-root hoisting (Task 20) — clarified.
- Snapshot fixtures (Task 31) — documented as: generated by `samply record /bin/ls --save-only`; checked-in if small.
