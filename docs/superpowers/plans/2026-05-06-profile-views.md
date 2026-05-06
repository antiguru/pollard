# Profile views implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add lazy "view" profiles to pollard so users can derive transformed slices (hide frames, collapse recursion, merge symbols) of an existing profile without materializing a new profile, and then drive every existing query tool against the view via the same `profile_id` plumbing.

**Architecture:** A view is `(base_profile_id, Transforms)` registered as a regular session that shares the base's `Arc<RawProfile>` but carries a non-default `Transforms` field on its `Profile`.
Aggregators stop calling `walk_stack` + `frame_info` directly and route through one new `Profile::resolved_chain(handle, stack_idx, expand_inlines)` helper that applies the transforms in-place.
Base profiles use the identity transform, so behavior is unchanged when no view is in play.

**Tech Stack:** Rust, rmcp tool framework, existing `Profile` / `ProfileSession` / `SessionRegistry`.

---

## Background

Today every query reads `Profile` directly.
A user that wants to hide e.g. tokio runtime frames must filter at the response layer or chain a regex `paths_to` argument.
The desired UX is: build a derived "view" once, then call `top_functions`, `call_tree`, `stacks_containing`, `compare_profiles`, and so on against the view's id and get pre-transformed answers everywhere.

The supported transforms in this plan:

* **`hide_frames`** — drop frames whose function name matches a substring or `re:` regex.
* **`hide_modules`** — drop frames whose module name matches.
* **`collapse_recursion`** — dedup consecutive frames sharing `(function, module)` so a self-recursive call shows up once per stack.
* **`merge_functions`** — rename function names via `re:pattern => replacement` rules; lets users fuse symbol variants (`foo<T>::bar` → `foo::bar`) so aggregation sees one symbol.

`focus_root` and `paths_to` already exist on `call_tree`; out of scope for this plan to avoid duplicating semantics.

## File structure

* Create: `src/profile/transforms.rs` — `Transforms` value type plus pre-compiled matcher / rename-rule structures. Pure data + apply logic. Read-only.
* Modify: `src/profile/parsed.rs` — embed `transforms` field on `Profile`, switch `raw` to `Arc<RawProfile>`, add `Profile::view(base, transforms)` constructor, add `Profile::resolved_chain(...)` helper.
* Modify: `src/profile/mod.rs` — re-export the new types.
* Modify: `src/session.rs` — add `ProfileSession::view(base, view_id, name, transforms)` constructor; thread the lib-outcomes through from base unchanged.
* Modify: `src/registry.rs` — `create_view` method; view ids live in the same map; eviction must keep a view's base alive while the view is loaded.
* Create: `src/tools/views.rs` — MCP tool wrappers (`create_view`, `unload_view` is just `unload_profile`, but list and describe need view-awareness).
* Modify: `src/tools/mod.rs` — register `views_router` next to lifecycle/query/drill_down.
* Modify: `src/tools/lifecycle.rs` — `list_profiles` reports view membership; `describe_profile` echoes transforms when set.
* Modify: query files that currently call `walk_stack` directly to call `resolved_chain` instead:
  * `src/query/top_functions.rs`
  * `src/query/call_tree.rs`
  * `src/query/stacks_containing.rs`
  * `src/query/folded.rs`
  * `src/query/top_groups.rs`
  * `src/query/summary.rs` (uses `top_functions` already, so transitive)
  * `src/query/compare.rs` and `src/query/compare_functions.rs`
* Create: `tests/views_e2e.rs` — load a known fixture, create a view that hides one function, assert downstream tools see the transformed counts.

Each file has one responsibility.
The `transforms.rs` module owns the *what*; `parsed.rs` owns the *how* by being the only place that walks the raw tables.

---

## Self-review checklist anchors

When the implementor finishes, the following must all be true:

* `cargo test --workspace` passes.
* The MVP fixture's `top_functions` against a base profile returns the same output as before this PR.
* The same fixture against a view that hides the leaf returns counts attributed to the next-up frame.
* No bespoke `walk_stack` + `frame_info` resolve loops remain in `src/query/*.rs`.

---

## Task 1: `Transforms` value type

**Files:**

* Create: `src/profile/transforms.rs`
* Modify: `src/profile/mod.rs` — add `pub mod transforms;` and re-export `Transforms`.

We isolate the data definition first so later tasks can wire it through `Profile` without churn.

* [ ] **Step 1: Add the empty module to `mod.rs`.**

Insert next to the existing module declarations.

```rust
pub mod transforms;
pub use transforms::Transforms;
```

* [ ] **Step 2: Write the failing unit test.**

`src/profile/transforms.rs`:

```rust
//! Lazy profile-view transforms applied to resolved frame chains.

#![allow(dead_code)]

use crate::matching::FunctionMatcher;

/// Frame-chain transformations applied lazily during query aggregation.
///
/// Default value is the identity transform; base profiles always carry
/// the default so existing query behavior is unchanged.
#[derive(Debug, Default, Clone)]
pub struct Transforms {
    /// Compiled function-name matchers. Frames whose `function_name`
    /// matches any entry are dropped from the chain *before* aggregation.
    pub hide_frames: Vec<FunctionMatcher>,
    /// Compiled module-name matchers. Frames whose `module_name` matches
    /// any entry are dropped from the chain.
    pub hide_modules: Vec<FunctionMatcher>,
    /// When true, runs of consecutive frames sharing
    /// `(function_name, module_name)` collapse to a single frame in the
    /// resolved chain.
    pub collapse_recursion: bool,
    /// Rename rules applied to `function_name` after hide filters and
    /// before recursion collapse.
    pub rename: Vec<RenameRule>,
}

#[derive(Debug, Clone)]
pub struct RenameRule {
    /// Compiled matcher that decides whether a frame is renamed.
    pub matcher: FunctionMatcher,
    /// Replacement string. Always literal — no capture-group interpolation
    /// in v1, since `merge_functions` is for symbol fusion, not regex
    /// templating.
    pub replacement: String,
}

impl Transforms {
    pub fn is_identity(&self) -> bool {
        self.hide_frames.is_empty()
            && self.hide_modules.is_empty()
            && !self.collapse_recursion
            && self.rename.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity() {
        assert!(Transforms::default().is_identity());
    }
}
```

* [ ] **Step 3: Run the test.**

Run: `cargo test --lib profile::transforms`
Expected: PASS (1 test).

* [ ] **Step 4: Commit.**

```bash
git add src/profile/mod.rs src/profile/transforms.rs
git commit -m "feat(profile): introduce Transforms value type for views"
```

---

## Task 2: Share `RawProfile` via `Arc` and add `transforms` field

**Files:**

* Modify: `src/profile/parsed.rs`

Today `Profile` owns `RawProfile` by value.
Views need to share the base's raw tables without copying.
We swap `raw: RawProfile` for `raw: Arc<RawProfile>` and add the `transforms: Transforms` field, defaulted to identity.

* [ ] **Step 1: Update the struct.**

In `src/profile/parsed.rs`, change the `Profile` struct:

```rust
pub struct Profile {
    raw: std::sync::Arc<crate::profile::raw::RawProfile>,
    /// Frame-chain transforms applied lazily by `resolved_chain`.
    /// Default = identity; views construct with non-default transforms
    /// via [`Self::view`].
    transforms: crate::profile::transforms::Transforms,
    /// Flattened (process, thread) tuples for top-level enumeration.
    threads: Vec<ThreadHandle>,
}
```

* [ ] **Step 2: Update `from_raw` and add `view`.**

Replace the existing `from_raw` body with:

```rust
impl Profile {
    pub fn from_raw(raw: crate::profile::raw::RawProfile) -> Self {
        Self::new_inner(std::sync::Arc::new(raw), crate::profile::transforms::Transforms::default())
    }

    /// Build a view profile that shares the base's raw tables but
    /// applies its own transforms. The thread enumeration is identical
    /// to the base — views never add or remove threads.
    pub fn view(base: &Self, transforms: crate::profile::transforms::Transforms) -> Self {
        Self::new_inner(std::sync::Arc::clone(&base.raw), transforms)
    }

    fn new_inner(
        raw: std::sync::Arc<crate::profile::raw::RawProfile>,
        transforms: crate::profile::transforms::Transforms,
    ) -> Self {
        let mut threads = Vec::new();
        for (i, _) in raw.threads.iter().enumerate() {
            threads.push(ThreadHandle {
                process: ProcessHandle { pid: 0, process_idx: None },
                thread_idx: i,
            });
        }
        for (pi, p) in raw.processes.iter().enumerate() {
            for (i, t) in p.threads.iter().enumerate() {
                threads.push(ThreadHandle {
                    process: ProcessHandle { pid: t.pid.value, process_idx: Some(pi) },
                    thread_idx: i,
                });
            }
        }
        Self { raw, transforms, threads }
    }

    /// Returns the transform set applied by `resolved_chain`. Identity
    /// for base profiles.
    pub fn transforms(&self) -> &crate::profile::transforms::Transforms {
        &self.transforms
    }
}
```

* [ ] **Step 3: Fix the `raw_thread` accessor.**

The `Arc` deref is automatic for `&self.raw.threads[...]`, no change needed; verify with `cargo check`.

Run: `cargo check`
Expected: PASS.

* [ ] **Step 4: Add a unit test for view sharing.**

Add at the bottom of the existing `mod tests` in `parsed.rs`:

```rust
#[test]
fn view_shares_raw_tables_and_threads() {
    let base = fixture();
    let view = Profile::view(&base, crate::profile::transforms::Transforms::default());
    assert_eq!(base.threads().count(), view.threads().count());
    assert!(view.transforms().is_identity());
    // Same Arc backing → same raw pointer.
    assert!(std::sync::Arc::ptr_eq(&base.raw, &view.raw));
}
```

* [ ] **Step 5: Run tests.**

Run: `cargo test --lib profile::parsed`
Expected: PASS (3 tests including the new one).

* [ ] **Step 6: Commit.**

```bash
git add src/profile/parsed.rs
git commit -m "feat(profile): Arc-share raw tables and add transforms field"
```

---

## Task 3: `ResolvedFrame` and `Profile::resolved_chain`

**Files:**

* Modify: `src/profile/parsed.rs`

The new helper resolves a stack-table index into a transformed `Vec<ResolvedFrame>` (root-to-leaf).
This is the single seam where transforms apply.
Returning a `Vec` (not `impl Iterator`) keeps lifetimes simple and matches what the call_tree / top_functions loops already build.

* [ ] **Step 1: Write the failing test.**

Add at the bottom of `mod tests` in `parsed.rs`:

```rust
#[test]
fn resolved_chain_with_identity_matches_walk_stack() {
    use crate::profile::raw::RawProfile;
    let raw: RawProfile =
        serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
    let p = Profile::from_raw(raw);
    let handle = p.threads().next().unwrap().handle();
    let stack_idx = p
        .stack_indices(handle, &crate::profile::event_source::EventSource::Samples, None)
        .find_map(|s| s)
        .unwrap();

    let manual: Vec<String> = p
        .walk_stack(handle, stack_idx)
        .filter_map(|fi| p.frame_info(handle, fi).map(|i| i.function_name.to_owned()))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()  // walk_stack is leaf-to-root; resolved_chain is root-to-leaf.
        .collect();
    let resolved: Vec<String> = p
        .resolved_chain(handle, stack_idx, false)
        .into_iter()
        .map(|f| f.function)
        .collect();
    assert_eq!(manual, resolved);
}

#[test]
fn resolved_chain_hides_matching_function() {
    use crate::matching::FunctionMatcher;
    use crate::profile::raw::RawProfile;
    use crate::profile::transforms::Transforms;
    let raw: RawProfile =
        serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
    let base = Profile::from_raw(raw);
    let mut t = Transforms::default();
    t.hide_frames.push(FunctionMatcher::substring("hot"));
    let view = Profile::view(&base, t);
    let handle = view.threads().next().unwrap().handle();
    for stack_opt in view.stack_indices(handle, &crate::profile::event_source::EventSource::Samples, None) {
        let Some(stack_idx) = stack_opt else { continue };
        let names: Vec<String> = view
            .resolved_chain(handle, stack_idx, false)
            .into_iter()
            .map(|f| f.function)
            .collect();
        assert!(!names.iter().any(|n| n == "hot"), "found hidden frame: {names:?}");
    }
}

#[test]
fn resolved_chain_collapses_consecutive_recursion() {
    // synthetic: build a 3-deep recursion of the same function and
    // expect resolved_chain to flatten it to one frame when collapse is on.
    use crate::profile::raw::RawProfile;
    use crate::profile::transforms::Transforms;
    let raw: RawProfile =
        serde_json::from_str(include_str!("../../tests/fixtures/recursive_three.json")).unwrap();
    let base = Profile::from_raw(raw);
    let view = Profile::view(&base, Transforms { collapse_recursion: true, ..Default::default() });
    let handle = view.threads().next().unwrap().handle();
    let stack_idx = view
        .stack_indices(handle, &crate::profile::event_source::EventSource::Samples, None)
        .find_map(|s| s)
        .unwrap();
    let names: Vec<String> = view
        .resolved_chain(handle, stack_idx, false)
        .into_iter()
        .map(|f| f.function)
        .collect();
    assert_eq!(names, vec!["main".to_owned(), "recurse".to_owned()]);
}
```

If `tests/fixtures/recursive_three.json` does not exist, build it with the synthetic helper before the test runs.
Add this fixture-builder once at the top of the test module:

```rust
#[ctor::ctor]
fn ensure_recursive_fixture() {
    let path = std::path::Path::new("tests/fixtures/recursive_three.json");
    if path.exists() {
        return;
    }
    // Three frames: main → recurse → recurse → recurse, single sample.
    let json = r#"{
      "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
      "libs": [],
      "threads": [{
        "name": "Main", "tid": 1, "pid": 1, "registerTime": 0.0,
        "stringArray": ["main", "recurse"],
        "frameTable": {"length": 2, "address": [-1, -1], "func": [0, 1], "category": [0, 0], "subcategory": [0, 0], "line": [null, null], "column": [null, null], "nativeSymbol": [null, null]},
        "stackTable": {"length": 4, "frame": [0, 1, 1, 1], "category": [0, 0, 0, 0], "subcategory": [0, 0, 0, 0], "prefix": [null, 0, 1, 2]},
        "samples": {"length": 1, "stack": [3], "time": [0.0]},
        "funcTable": {"length": 2, "name": [0, 1], "isJS": [false, false], "relevantForJS": [false, false], "resource": [-1, -1], "fileName": [null, null], "lineNumber": [null, null], "columnNumber": [null, null]},
        "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
        "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
      }]
    }"#;
    std::fs::write(path, json).unwrap();
}
```

If `ctor` isn't already a dev-dependency, prefer adding the fixture as a static `include_str!` in the test rather than a separate file — the recursive fixture is small and lives only in this one test.
Inline alternative that avoids a new dev-dep: drop the `ensure_recursive_fixture` helper and load the JSON via `serde_json::from_str` against a `let json = r#"..."#;` string literal inside the test body.

Use the inline literal approach to keep the dev-dep set unchanged.

* [ ] **Step 2: Run tests to verify they fail with "method not found".**

Run: `cargo test --lib profile::parsed::tests`
Expected: FAIL with `no method named resolved_chain`.

* [ ] **Step 3: Implement `ResolvedFrame` and `resolved_chain`.**

Add to `parsed.rs`:

```rust
#[derive(Debug, Clone)]
pub struct ResolvedFrame {
    pub function: String,
    pub module: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub address: Option<i64>,
}

impl Profile {
    /// Resolve `stack_idx` into a frame chain (root-to-leaf), applying
    /// the profile's transforms. When `expand_inlines` is true, native
    /// frames fan out into their DWARF inline chain (outer-to-inner)
    /// before transforms apply, so hide/rename rules see inline frames
    /// as first-class entries.
    pub fn resolved_chain(
        &self,
        handle: ThreadHandle,
        stack_idx: usize,
        expand_inlines: bool,
    ) -> Vec<ResolvedFrame> {
        // walk_stack iterates leaf-to-root; collect-then-reverse keeps
        // order explicit (root-to-leaf) so transforms downstream see
        // chains in execution order.
        let leaf_first: Vec<usize> = self.walk_stack(handle, stack_idx).collect();
        let mut chain: Vec<ResolvedFrame> = Vec::with_capacity(leaf_first.len());
        for &fi in leaf_first.iter().rev() {
            let Some(info) = self.frame_info(handle, fi) else { continue };
            chain.push(ResolvedFrame {
                function: info.function_name.to_owned(),
                module: info.module_name.map(str::to_owned),
                file: info.file.map(str::to_owned),
                line: info.line,
                column: info.column,
                address: info.address,
            });
            if expand_inlines {
                // Inline chain is innermost-first; emit outer-to-inner
                // so the deepest inline becomes the leaf.
                let module = info.module_name.map(str::to_owned);
                for inl in self.inline_chain(handle, fi).iter().rev() {
                    chain.push(ResolvedFrame {
                        function: inl.function.clone(),
                        module: module.clone(),
                        file: inl.file.clone(),
                        line: inl.line,
                        column: None,
                        address: None,
                    });
                }
            }
        }
        self.apply_transforms(&mut chain);
        chain
    }

    fn apply_transforms(&self, chain: &mut Vec<ResolvedFrame>) {
        let t = &self.transforms;
        if t.is_identity() {
            return;
        }
        // Hide first so rename and collapse don't see frames the user
        // told us to drop.
        if !t.hide_frames.is_empty() || !t.hide_modules.is_empty() {
            chain.retain(|f| {
                if t.hide_frames.iter().any(|m| m.matches(&f.function)) {
                    return false;
                }
                if let Some(m) = f.module.as_deref() {
                    if t.hide_modules.iter().any(|mm| mm.matches(m)) {
                        return false;
                    }
                }
                true
            });
        }
        if !t.rename.is_empty() {
            for f in chain.iter_mut() {
                for rule in &t.rename {
                    if rule.matcher.matches(&f.function) {
                        f.function = rule.replacement.clone();
                        break;
                    }
                }
            }
        }
        if t.collapse_recursion {
            chain.dedup_by(|a, b| a.function == b.function && a.module == b.module);
        }
    }
}
```

* [ ] **Step 4: Run tests.**

Run: `cargo test --lib profile::parsed::tests`
Expected: PASS (all tests including the three new ones).

* [ ] **Step 5: Commit.**

```bash
git add src/profile/parsed.rs
git commit -m "feat(profile): resolved_chain helper applies view transforms"
```

---

## Task 4: Adopt `resolved_chain` in `top_functions` aggregator

**Files:**

* Modify: `src/query/top_functions.rs`

`aggregate_grouped` (used by `top_functions`, `top_groups`, and `compare`) currently inlines its own `walk_stack` + `frame_info` loop.
Switch it to call `Profile::resolved_chain`, deleting the duplicated logic.

* [ ] **Step 1: Replace the inner loop.**

In `aggregate_grouped`, replace the `for handle in filter_args.threads(profile)` body's frame-resolution block with a call into `resolved_chain`.
Concretely, replace the section that builds `entries: Vec<(String, Option<String>, Option<String>)>` (the leaf-to-root chain with file paths) with:

```rust
for handle in filter_args.threads(profile) {
    for stack_opt in profile.stack_indices(handle, event, filter_args.time_range) {
        let Some(stack_idx) = stack_opt else { continue };
        total_samples += 1;
        // Root-to-leaf, transforms already applied. Reverse to keep the
        // existing leaf-to-root iteration the rest of this function
        // expects (self-time goes to the last frame walked).
        let mut entries: Vec<ResolvedFrame> =
            profile.resolved_chain(handle, stack_idx, expand_inlines);
        if entries.is_empty() {
            continue;
        }
        entries.reverse(); // leaf-to-root, matches the previous shape
        // ... existing aggregation logic, now reading f.function / f.module / f.file
    }
}
```

Adjust the per-frame extraction inside the loop to read from `ResolvedFrame` (`f.function`, `f.module`, `f.file`) instead of the previous tuple shape.
Update the `key_fn` callsites accordingly: they previously received `(name, module, file)` from the inline resolver, now from the `ResolvedFrame`.

* [ ] **Step 2: Confirm tests still pass.**

Run: `cargo test --lib query::top_functions`
Expected: PASS.

* [ ] **Step 3: Commit.**

```bash
git add src/query/top_functions.rs
git commit -m "refactor(query): top_functions consumes resolved_chain"
```

---

## Task 5: Adopt `resolved_chain` in `call_tree`

**Files:**

* Modify: `src/query/call_tree.rs`

`accumulate_with_root` walks frames manually; replace with `resolved_chain` and drop the inline-chain handling that has now moved into the helper.

* [ ] **Step 1: Refactor `accumulate_with_root`.**

Replace the body of the `for stack_opt in profile.stack_indices(...)` loop with:

```rust
for stack_opt in profile.stack_indices(handle, event, time_range) {
    let Some(stack_idx) = stack_opt else { continue };
    // Root-to-leaf already; transforms applied.
    let mut frames: Vec<(String, Option<String>)> = profile
        .resolved_chain(handle, stack_idx, expand_inlines)
        .into_iter()
        .map(|f| (f.function, f.module))
        .collect();
    if frames.is_empty() {
        continue;
    }
    if inverted {
        frames.reverse();
    }
    // ...existing paths_to / root_function / aggregation logic unchanged.
}
```

Delete the block that walked `walk_stack` + `inline_chain` to build `frames`.

* [ ] **Step 2: Run call_tree tests.**

Run: `cargo test --lib query::call_tree`
Expected: PASS.

* [ ] **Step 3: Commit.**

```bash
git add src/query/call_tree.rs
git commit -m "refactor(query): call_tree consumes resolved_chain"
```

---

## Task 6: Adopt `resolved_chain` in remaining query files

**Files:**

* Modify: `src/query/stacks_containing.rs`
* Modify: `src/query/folded.rs`
* Modify: `src/query/top_groups.rs` (delegate to `aggregate_grouped`; verify the diff is mechanical)
* Modify: `src/query/compare.rs`
* Modify: `src/query/compare_functions.rs`

Each of these has its own walk; the refactor pattern is the one used in Tasks 4 and 5.

* [ ] **Step 1: Convert each file in turn, running `cargo test --lib query::<modname>` between conversions to keep the diffs small.**

For each file:

1. Replace the manual frame-resolution loop with `profile.resolved_chain(...)`.
2. If the file built a `Vec<FrameOutput>` for the response, derive that from `ResolvedFrame` directly.
3. Run the file's unit tests.
4. Commit each conversion separately so a regression is bisectable:

```bash
git add src/query/stacks_containing.rs
git commit -m "refactor(query): stacks_containing consumes resolved_chain"
```

(Repeat for the other four files.)

* [ ] **Step 2: Run the full library test suite.**

Run: `cargo test --lib`
Expected: PASS.

---

## Task 7: View id scheme and `SessionRegistry::create_view`

**Files:**

* Modify: `src/registry.rs`
* Modify: `src/session.rs`

Each view shares its base's `Arc<Profile>` but installs its own transforms.
Ids are deterministic on `(base_id, transforms)` so re-creating the same view returns the same id.

* [ ] **Step 1: Add a view constructor on `ProfileSession`.**

In `src/session.rs`:

```rust
impl ProfileSession {
    /// Build a derived session that shares the base's raw tables but
    /// applies its own transforms. Lib outcomes are inherited verbatim.
    pub fn view(
        base: &ProfileSession,
        view_id: String,
        name: String,
        transforms: crate::profile::transforms::Transforms,
    ) -> Self {
        let view_profile = std::sync::Arc::new(crate::profile::Profile::view(
            base.profile(),
            transforms,
        ));
        Self {
            id: view_id,
            name,
            // Path is the *base* path; views are not on disk but reusing
            // the base path keeps `re-load by path` working when the view
            // is evicted (re-loading drops the view, restores the base).
            path: base.path().to_path_buf(),
            profile: view_profile,
            unsymbolicated_pct: base.unsymbolicated_pct(),
            lib_outcomes: base.lib_outcomes().to_vec(),
        }
    }
}
```

Note: this requires `lib_outcomes` to be `Clone`-able; verify it already derives `Clone` and add the derive if not.

* [ ] **Step 2: Add `create_view` on `SessionRegistry`.**

```rust
impl SessionRegistry {
    /// Build a derived view session over `base_id`, sharing the base's
    /// raw tables but applying `transforms`. Returns the new view id and
    /// any sessions evicted to make room.
    ///
    /// Errors with `ProfileNotFound` / `ProfileEvicted` if the base is
    /// not currently loaded (we don't auto-reload — that would block the
    /// caller on re-symbolication without warning).
    pub async fn create_view(
        &self,
        base_id: &str,
        name: Option<&str>,
        transforms: crate::profile::transforms::Transforms,
    ) -> Result<(String, Vec<EvictedSession>), ToolError> {
        let base = self.get_or_error(base_id).await?;
        let view_id = view_id_from(base_id, &transforms);
        let view_name = name
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}#view", base.name()));
        let session = ProfileSession::view(&base, view_id.clone(), view_name, transforms);
        let mut inner = self.inner.write().await;
        if inner.sessions.contains_key(&view_id) {
            inner.order.retain(|x| x != &view_id);
        }
        let mut evicted = Vec::new();
        while inner.sessions.len() >= self.capacity {
            let Some(victim_id) = inner.order.pop_front() else { break };
            // Don't evict the base out from under a view we're about to
            // register — bases must outlive their derived views.
            if victim_id == base_id {
                inner.order.push_front(victim_id);
                break;
            }
            let Some(s) = inner.sessions.remove(&victim_id) else { continue };
            let entry = EvictedSession {
                profile_id: victim_id.clone(),
                name: s.name().to_owned(),
                path: s.path().to_path_buf(),
            };
            inner.evicted.insert(victim_id, entry.clone());
            evicted.push(entry);
        }
        inner.evicted.remove(&view_id);
        inner.order.push_back(view_id.clone());
        inner.sessions.insert(view_id.clone(), Arc::new(session));
        Ok((view_id, evicted))
    }
}

fn view_id_from(base_id: &str, transforms: &crate::profile::transforms::Transforms) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    base_id.hash(&mut h);
    // Hash a compact, stable representation of transforms. We Debug-print
    // them: the Debug impl is derived and stable across releases on the
    // same struct shape; re-add an explicit hash if we ever need
    // cross-process compatibility (we don't today).
    format!("{transforms:?}").hash(&mut h);
    format!("{base_id}.v{:08x}", h.finish() as u32)
}
```

* [ ] **Step 3: Test `create_view`.**

Add to the existing `mod tests` in `registry.rs`:

```rust
#[tokio::test]
async fn create_view_returns_deterministic_id() {
    let registry = SessionRegistry::new(2);
    let (base_id, _) = registry
        .load(std::path::Path::new("tests/fixtures/two_functions.json"), None)
        .await
        .unwrap();
    let (view_id_1, _) = registry
        .create_view(&base_id, None, Default::default())
        .await
        .unwrap();
    let (view_id_2, _) = registry
        .create_view(&base_id, None, Default::default())
        .await
        .unwrap();
    assert_eq!(view_id_1, view_id_2, "same transforms should yield same view id");
    assert_ne!(view_id_1, base_id);
}

#[tokio::test]
async fn create_view_does_not_evict_its_base() {
    let registry = SessionRegistry::new(1);
    let (base_id, _) = registry
        .load(std::path::Path::new("tests/fixtures/two_functions.json"), None)
        .await
        .unwrap();
    let (view_id, evicted) = registry
        .create_view(&base_id, None, Default::default())
        .await
        .unwrap();
    // capacity=1, but the base must remain so the view can read it.
    assert!(registry.get(&base_id).await.is_some(), "base must stay loaded under a view");
    assert!(registry.get(&view_id).await.is_some());
    assert!(evicted.is_empty(), "no eviction expected; we keep the base");
}
```

* [ ] **Step 4: Run tests.**

Run: `cargo test --lib registry`
Expected: PASS.

* [ ] **Step 5: Commit.**

```bash
git add src/registry.rs src/session.rs
git commit -m "feat(registry): create_view derives a session from a base"
```

---

## Task 8: `create_view` MCP tool

**Files:**

* Create: `src/tools/views.rs`
* Modify: `src/tools/mod.rs`

The MCP surface gets one new tool. `unload_profile` already works for views — no separate wrapper needed.

* [ ] **Step 1: Write the tool wrapper.**

`src/tools/views.rs`:

```rust
//! `create_view` MCP tool: derive a transformed lazy view of a profile.

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::transforms::{RenameRule, Transforms};
use crate::tools::PollardServer;
use crate::tools::lifecycle::{EvictedRef, ProfileDescription};
use rmcp::Json;
use rmcp::handler::server::tool::Parameters;
use rmcp::tool;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateViewArgs {
    /// Profile id to derive from. Use `list_profiles` to list candidates.
    pub profile_id: String,
    /// Optional human-readable label. Defaults to `<base name>#view`.
    #[serde(default)]
    pub name: Option<String>,
    /// Frames whose function name matches any pattern are dropped.
    /// Substring by default; prefix with `re:` for a regex.
    #[serde(default)]
    pub hide_frames: Vec<String>,
    /// Frames whose module name matches any pattern are dropped.
    #[serde(default)]
    pub hide_modules: Vec<String>,
    /// When true, runs of consecutive same-symbol frames collapse to one
    /// frame in every aggregation.
    #[serde(default)]
    pub collapse_recursion: bool,
    /// Function-name rename rules. Each entry must be `re:<pattern> => <replacement>`
    /// (the `re:` prefix is required to keep parsing unambiguous).
    #[serde(default)]
    pub rename: Vec<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CreateViewResult {
    pub profile_id: String,
    pub description: ProfileDescription,
    pub evicted: Vec<EvictedRef>,
}

#[rmcp::tool_router(router = views_router, vis = pub)]
impl PollardServer {
    #[tool(
        description = "Create a derived view profile that lazily transforms an existing profile. \
        Returns a new `profile_id` you can pass to any other tool. Views share the base profile's \
        raw tables (no extra memory cost) and apply transforms during aggregation: hide frames by \
        name or module, collapse consecutive recursion, and merge symbols via rename rules. \
        Re-creating the same view returns the same id; unload_profile frees a view without \
        touching the base."
    )]
    pub async fn create_view(
        &self,
        Parameters(args): Parameters<CreateViewArgs>,
    ) -> Result<Json<CreateViewResult>, rmcp::ErrorData> {
        let transforms = build_transforms(&args)?;
        let (id, evicted) = self
            .registry
            .create_view(&args.profile_id, args.name.as_deref(), transforms)
            .await?;
        let session = self
            .registry
            .get(&id)
            .await
            .ok_or_else(|| rmcp::ErrorData::internal_error("view vanished after create", None))?;
        let mut desc = crate::query::describe::describe(
            session.profile(),
            session.id(),
            session.name(),
            &session.path().display().to_string(),
            session.unsymbolicated_pct(),
            crate::tools::lifecycle::DEFAULT_TOP_N,
        );
        desc.lib_diagnostics =
            crate::tools::lifecycle::problematic_outcomes(session.lib_outcomes());
        let evicted = evicted
            .into_iter()
            .map(|e| EvictedRef {
                profile_id: e.profile_id,
                name: e.name,
                path: e.path.display().to_string(),
            })
            .collect();
        Ok(Json(CreateViewResult { profile_id: id, description: desc, evicted }))
    }
}

fn build_transforms(args: &CreateViewArgs) -> Result<Transforms, ToolError> {
    let hide_frames = args
        .hide_frames
        .iter()
        .map(|s| FunctionMatcher::parse("hide_frames", s))
        .collect::<Result<Vec<_>, _>>()?;
    let hide_modules = args
        .hide_modules
        .iter()
        .map(|s| FunctionMatcher::parse("hide_modules", s))
        .collect::<Result<Vec<_>, _>>()?;
    let rename = args
        .rename
        .iter()
        .map(parse_rename_rule)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Transforms {
        hide_frames,
        hide_modules,
        collapse_recursion: args.collapse_recursion,
        rename,
    })
}

fn parse_rename_rule(raw: &str) -> Result<RenameRule, ToolError> {
    // Required form: "re:<pattern> => <replacement>". The `re:` prefix
    // is mandatory in v1 — substring renames aren't useful enough to
    // justify a second syntax.
    let Some(rest) = raw.strip_prefix("re:") else {
        return Err(ToolError::InvalidArgument {
            field: "rename".into(),
            message: format!("rename rule must start with `re:`, got {raw:?}"),
        });
    };
    let Some((pattern, replacement)) = rest.split_once(" => ") else {
        return Err(ToolError::InvalidArgument {
            field: "rename".into(),
            message: format!("rename rule must contain ` => `, got {raw:?}"),
        });
    };
    Ok(RenameRule {
        matcher: FunctionMatcher::parse("rename", &format!("re:{pattern}"))?,
        replacement: replacement.to_owned(),
    })
}
```

(Adjust to the actual public surface of `FunctionMatcher` and `ToolError::InvalidArgument` — match whatever exists in `src/matching.rs` / `src/error.rs`. If the matcher has a different `parse` signature, follow the existing convention.)

* [ ] **Step 2: Wire the router.**

In `src/tools/mod.rs`:

```rust
pub mod drill_down;
pub mod lifecycle;
pub mod query;
pub mod views;

impl PollardServer {
    pub fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::lifecycle_router()
            + Self::query_router()
            + Self::drill_down_router()
            + Self::views_router()
    }
}
```

* [ ] **Step 3: Cargo check.**

Run: `cargo check`
Expected: PASS.

* [ ] **Step 4: Commit.**

```bash
git add src/tools/mod.rs src/tools/views.rs
git commit -m "feat(tools): create_view MCP tool for derived profiles"
```

---

## Task 9: `list_profiles` reports view membership

**Files:**

* Modify: `src/tools/lifecycle.rs`
* Modify: `src/registry.rs`

`LoadedProfile` should include an optional `base_profile_id` so a caller can spot which entries are views.

* [ ] **Step 1: Track base id on `ProfileSession`.**

Add a `base_id: Option<String>` field to `ProfileSession`, populated in `view(...)` from `base.id()` and `None` in `load(...)`.
Expose via `pub fn base_id(&self) -> Option<&str>`.

* [ ] **Step 2: Surface it on `LoadedProfile`.**

In `src/tools/lifecycle.rs`, add `pub base_profile_id: Option<String>` to `LoadedProfile` and populate from `s.base_id().map(String::from)` in `list_profiles`.

* [ ] **Step 3: Add a smoke test in `tests/mcp_integration.rs`.**

The test creates a base profile, derives a view, and asserts `list_profiles` reports the view's `base_profile_id`.
Use the same fixture-loading helper the file already uses.

* [ ] **Step 4: Run the integration test.**

Run: `cargo test --test mcp_integration`
Expected: PASS.

* [ ] **Step 5: Commit.**

```bash
git add src/session.rs src/tools/lifecycle.rs tests/mcp_integration.rs
git commit -m "feat(tools): list_profiles reports view base id"
```

---

## Task 10: End-to-end view test

**Files:**

* Create: `tests/views_e2e.rs`

A single integration test exercises the full chain: load → create_view (hide a leaf) → call `top_functions` against the view → assert the leaf's samples re-attribute upward.

* [ ] **Step 1: Write the test.**

```rust
//! End-to-end check that view transforms propagate to every aggregator.

use pollard::profile::{Profile, transforms::Transforms};
use pollard::matching::FunctionMatcher;
use pollard::query::top_functions::{Args, top_functions};

#[test]
fn hidden_leaf_re_attributes_self_time_to_caller() {
    let raw: pollard::profile::raw::RawProfile = serde_json::from_str(include_str!(
        "fixtures/two_functions.json"
    ))
    .unwrap();
    let base = Profile::from_raw(raw);
    let baseline = top_functions(&base, &Args::default()).unwrap();
    let leaf_self = baseline
        .functions
        .iter()
        .find(|f| f.function == "hot")
        .map(|f| f.self_samples)
        .unwrap_or(0);
    assert!(leaf_self > 0, "fixture should give `hot` measurable self-time");

    let mut t = Transforms::default();
    t.hide_frames.push(FunctionMatcher::substring("hot"));
    let view = Profile::view(&base, t);
    let result = top_functions(&view, &Args::default()).unwrap();
    assert!(
        result.functions.iter().all(|f| f.function != "hot"),
        "hidden frame must not appear in view aggregation: {:?}",
        result.functions
    );
    let next = result
        .functions
        .iter()
        .find(|f| f.function == "cold")
        .expect("caller of `hot` should be visible");
    assert!(
        next.self_samples >= leaf_self,
        "caller's self-time should absorb the hidden leaf's samples"
    );
}
```

* [ ] **Step 2: Run the test.**

Run: `cargo test --test views_e2e`
Expected: PASS.

* [ ] **Step 3: Commit.**

```bash
git add tests/views_e2e.rs
git commit -m "test(views): end-to-end view re-attributes hidden leaf"
```

---

## Task 11: Doc updates

**Files:**

* Modify: `docs/superpowers/specs/2026-04-28-pollard-design.md` — add a "Views" section under session management.
* Modify: `README.md` if it exposes the tool surface (check first; pollard's README may already be a brief).

Document, with examples:

* Lifecycle: `create_view` → derived `profile_id` → query like any other → `unload_profile` to drop just the view.
* Transform semantics in one paragraph each (hide / collapse / rename).
* That eviction protects the base while a view is loaded.

* [ ] **Step 1: Edit the spec.**

Add a "Profile views" subsection under "Session management" with the above bullets.

* [ ] **Step 2: Commit.**

```bash
git add docs/superpowers/specs/2026-04-28-pollard-design.md README.md
git commit -m "docs(spec): document profile views"
```

---

## Self-review

After Task 11:

1. **Spec coverage:** Hide-by-name (Task 1, 3, 8), hide-by-module (1, 3, 8), collapse recursion (1, 3, 8), merge symbols via rename (1, 3, 8), one-call API (8), shared sessions (7), all existing tools work via `resolved_chain` (4–6).
   Focus-subtree intentionally deferred — covered by `call_tree`'s existing `root_function` arg.
2. **Placeholder scan:** All steps cite real file paths. Code blocks are complete enough to copy-paste, modulo small adjustments to match the existing matcher / error API in `src/matching.rs` and `src/error.rs`. The `parse_rename_rule` and `build_transforms` helpers must read the existing constructors before adopting; this is called out in Task 8 step 1.
3. **Type consistency:** `Transforms` and `RenameRule` keep their shape across all tasks. `ResolvedFrame` is introduced in Task 3 and reused unchanged in Tasks 4–6. `view_id_from` always returns `<base>.v<hash>`.

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-06-profile-views.md`.
Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
