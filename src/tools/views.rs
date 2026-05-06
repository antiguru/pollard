//! `create_view` MCP tool: derive a transformed lazy view of a profile.

use crate::error::ToolError;
use crate::matching::{FunctionMatcher, matcher_to_string, required_matcher};
use crate::profile::symbolicate::problematic_outcomes;
use crate::profile::transforms::{RenameRule, Transforms};
use crate::query::describe::{DEFAULT_TOP_N, ProfileDescription, describe};
use crate::query::filters::{Filter, ProcessFilter, ThreadFilter};
use crate::query::view_stats::{RuleStat, ViewStats};
use crate::tools::PollardServer;
use crate::tools::lifecycle::EvictedRef;
use crate::tools::query::{CommonFilterArgs, parse_filter};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Input / output shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
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
    /// Substring by default; prefix with `re:` for a regex.
    #[serde(default)]
    pub hide_modules: Vec<String>,
    /// Inverse of `hide_frames`: only frames whose function name matches
    /// any pattern survive. Each maximal run of non-matching frames
    /// collapses into a single placeholder frame named `<hidden>`. ORs
    /// with `keep_only_modules` — a frame is kept if it matches any
    /// `keep_only_*` rule. Substring by default; prefix with `re:` for
    /// a regex. Applied before `hide_*`, so a frame matching both a
    /// `keep_only` and a `hide` rule is dropped.
    #[serde(default)]
    pub keep_only_frames: Vec<String>,
    /// Inverse of `hide_modules`: only frames whose module name matches
    /// any pattern survive. See `keep_only_frames` for OR semantics
    /// and placeholder behavior.
    #[serde(default)]
    pub keep_only_modules: Vec<String>,
    /// When true, repeating adjacent cycles in each stack collapse to one
    /// occurrence — `[A, B, C, A, B, C, X]` becomes `[A, B, C, X]`.
    /// Cycles up to length 8 are detected; the simple consecutive
    /// same-symbol case is just length 1.
    #[serde(default)]
    pub collapse_recursion: bool,
    /// When true, balanced `<…>` segments are removed from each
    /// frame's function name before `rename` rules fire — so
    /// `OrdValBatch<RowRowLayout<((Row, Row), Ts, i64)>>` becomes
    /// `OrdValBatch` and the rules can target the normalized name.
    /// Generic-bearing languages (Rust, C++ templates, C#, Java
    /// generics) all benefit; the trade-off is that any literal `<`
    /// or `>` in a symbol is also stripped.
    #[serde(default)]
    pub strip_type_params: bool,
    /// Function-name rename rules. Each entry must be `re:<pattern> => <replacement>`.
    /// The `re:` prefix is mandatory: substring renames aren't useful enough to
    /// justify a second syntax in v1. The replacement supports regex capture
    /// references — `$1`, `${name}` interpolate groups from the pattern, and
    /// a literal `$` is written `$$` — so a single rule can fold a
    /// trait-vs-inherent monomorphisation pair like
    /// `re:<(.*) as .*::Schedule>::schedule => $1::schedule`.
    #[serde(default)]
    pub rename: Vec<String>,
    /// View scope: pin a process / thread / time_range once at view
    /// creation so every downstream tool call inherits the slice
    /// without re-passing it. Per-call filters on the resulting view
    /// must be a sub-slice — any conflicting per-call filter is
    /// rejected with `invalid_value` so the caller can correct in one
    /// retry. Same syntax as the per-call filter args:
    /// `thread="tid:NNN"` or a thread name; `process="pid:NNN"` /
    /// `"pid:NNN.M"` or a process name; `time_range=[start_ms, end_ms]`
    /// relative to profile start.
    #[serde(flatten)]
    pub scope: CommonFilterArgs,
}

#[derive(Serialize, JsonSchema)]
pub struct CreateViewResult {
    pub profile_id: String,
    pub description: ProfileDescription,
    /// Per-rule diagnostic counts over the base profile's samples.
    /// A rule with `frames_matched: 0` is the typo signal — the
    /// pattern compiled but never matched anything in the profile.
    /// Includes parent rules when stacking views.
    pub rule_stats: Vec<RuleStat>,
    /// Total samples in the underlying base profile, the denominator
    /// for `samples_affected` shares.
    pub total_base_samples: u64,
    /// Profiles that were evicted from the in-memory cache to make room
    /// for this view. Empty when no eviction was needed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evicted: Vec<EvictedRef>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DescribeViewArgs {
    /// View profile id. Use `list_profiles` to find candidates;
    /// non-view profiles return `not_a_view`.
    pub profile_id: String,
}

#[derive(Serialize, JsonSchema)]
pub struct ViewPresetsResult {
    /// Markdown cookbook of canonical `hide_modules` / `hide_frames`
    /// regex sets for `create_view`. Source of truth lives in
    /// `docs/superpowers/specs/2026-05-06-view-presets-cookbook.md`
    /// and is embedded via `include_str!` so this tool always matches
    /// the doc shipped with the binary.
    pub cookbook: &'static str,
}#[derive(Serialize, JsonSchema)]
pub struct DescribeViewResult {
    pub profile_id: String,
    /// Immediate parent profile id. For a view stacked on another view
    /// this is the parent view, not the root base.
    pub base_profile_id: String,
    /// Composed transform shape — the full set of rules that fire when
    /// any tool reads this view, including rules inherited from
    /// parent views.
    pub transforms: TransformsView,
    /// Composed view scope (process / thread / time_range) inherited
    /// from every layer above this view. Omitted when no scope is set
    /// anywhere in the chain.
    #[serde(skip_serializing_if = "ScopeView::is_empty")]
    pub scope: ScopeView,
    /// Per-rule diagnostic counts. Same shape as `create_view`'s
    /// `rule_stats`; reused so callers can re-fetch later without
    /// re-creating the view.
    pub rule_stats: Vec<RuleStat>,
    pub total_base_samples: u64,
}

/// Stable wire shape for a view's pre-filter (process / thread /
/// time_range). Keeps `describe_view` JSON-stable when individual
/// fields are unset.
#[derive(Default, Serialize, JsonSchema)]
pub struct ScopeView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_range: Option<[f64; 2]>,
}

impl ScopeView {
    fn is_empty(&self) -> bool {
        self.thread.is_none() && self.process.is_none() && self.time_range.is_none()
    }
}

#[derive(Serialize, JsonSchema)]
pub struct TransformsView {
    /// Function-name patterns whose matching frames are dropped.
    pub hide_frames: Vec<String>,
    /// Module-name patterns whose matching frames are dropped.
    pub hide_modules: Vec<String>,
    /// Function-name patterns; only matching frames survive (with each
    /// maximal run of non-matching frames replaced by `<hidden>`).
    pub keep_only_frames: Vec<String>,
    /// Module-name patterns; only matching frames survive (with each
    /// maximal run of non-matching frames replaced by `<hidden>`).
    pub keep_only_modules: Vec<String>,
    /// Sequential rename rules.
    pub rename: Vec<RenameView>,
    pub collapse_recursion: bool,
    pub strip_type_params: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct RenameView {
    pub pattern: String,
    pub replacement: String,
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

#[tool_router(router = views_router, vis = "pub(crate)")]
impl PollardServer {
    #[tool(
        description = "Create a derived view profile that lazily transforms an existing profile. \
        Returns a new `profile_id` you can pass to any other tool. Views share the base profile's \
        raw tables (no extra memory cost) and apply transforms during aggregation: hide frames by \
        name or module, keep only frames matching a focus list (everything else collapses into a \
        `<hidden>` placeholder), collapse repeating cycles in stacks, and merge symbols via rename \
        rules. \
        Argument syntax: `hide_frames`, `hide_modules`, `keep_only_frames`, and `keep_only_modules` \
        use substring match by default; prefix a pattern with `re:` for a regex (e.g. `re:^tokio::`). \
        `keep_only_*` is the inverse of `hide_*`: only matching frames survive, and each maximal \
        run of non-matching frames collapses into a single `<hidden>` placeholder. `keep_only_*` \
        runs before `hide_*`, so a frame that matches both is dropped. \
        `rename` rules use the form `re:<pattern> => <replacement>` — the `re:` prefix is \
        required so the ` => ` separator is unambiguous. The replacement supports regex \
        capture references (`$1`, `${name}`); write `$$` for a literal `$`. \
        Set `collapse_recursion=true` when a recurrence dominates and each occurrence should \
        count as one. Repeating adjacent cycles up to length 8 collapse: `[A,B,C,A,B,C,X]` \
        becomes `[A,B,C,X]`. \
        Set `strip_type_params=true` to drop balanced `<…>` segments from frame names \
        before `rename` rules fire — `OrdValBatch<RowRowLayout<…>>` collapses to \
        `OrdValBatch`, so rules can target the normalized name. \
        Pass `process` / `thread` / `time_range` to pin a scope on the view — every downstream \
        tool call inherits the slice without re-passing it. Per-call filters on the resulting \
        view must be a sub-slice of the view scope (e.g. a `time_range`-scoped view accepts a \
        narrower per-call `time_range` but rejects a wider one). Same syntax as the per-call \
        filter args. \
        Re-creating the same view returns the same id; unload_profile frees a view without \
        touching the base. \
        Call `view_presets` for copy-paste `hide_modules` / `hide_frames` regex sets covering \
        common Rust noise (tracing-subscriber walls, tokio runtime internals, stdlib glue)."
    )]
    pub async fn create_view(
        &self,
        Parameters(args): Parameters<CreateViewArgs>,
    ) -> Result<Json<CreateViewResult>, rmcp::ErrorData> {
        let transforms = build_transforms(&args)?;
        let scope = parse_filter(&args.scope)?;
        // Best-effort up-front validation of the scope against the
        // immediate base — surfaces the same `thread_not_found` /
        // `process_not_found` / `out_of_bounds` error shape the caller
        // would otherwise see on the *next* query, but at the
        // create-view step where it's actionable. Intermediate parents
        // already validated their own scopes when they were created;
        // composition only narrows.
        if let Some(session) = self.registry.get(&args.profile_id).await {
            scope.validate_thread(session.profile())?;
            scope.validate_process(session.profile())?;
            scope.validate_time_range(session.profile())?;
        }
        let (id, evicted) = self
            .registry
            .create_view(&args.profile_id, args.name.as_deref(), transforms, scope)
            .await?;
        let session =
            self.registry.get(&id).await.ok_or_else(|| {
                rmcp::ErrorData::internal_error("view vanished after create", None)
            })?;
        let mut desc = describe(
            session.profile(),
            session.id(),
            session.name(),
            &session.path().display().to_string(),
            session.unsymbolicated_pct(),
            DEFAULT_TOP_N,
        );
        desc.lib_diagnostics = problematic_outcomes(session.lib_outcomes());
        let evicted = evicted
            .into_iter()
            .map(|e| EvictedRef {
                profile_id: e.profile_id,
                name: e.name,
                path: e.path.display().to_string(),
            })
            .collect();
        let stats = session
            .view_stats()
            .cloned()
            .unwrap_or_else(ViewStats::empty);
        Ok(Json(CreateViewResult {
            profile_id: id,
            description: desc,
            rule_stats: stats.rule_stats,
            total_base_samples: stats.total_base_samples,
            evicted,
        }))
    }

    #[tool(
        description = "Describe a previously-created view: parent base id, the full composed transform set, \
        and per-rule frames_matched / samples_affected counts over the base profile's samples. \
        Symmetric with `describe_profile` for loaded profiles. Use this to confirm a view's rules \
        are still firing as expected — `frames_matched: 0` typically signals a typo in the original \
        `create_view` call."
    )]
    pub async fn describe_view(
        &self,
        Parameters(args): Parameters<DescribeViewArgs>,
    ) -> Result<Json<DescribeViewResult>, rmcp::ErrorData> {
        let session = self.registry.get_or_error(&args.profile_id).await?;
        let Some(base_profile_id) = session.base_id().map(str::to_owned) else {
            return Err(ToolError::InvalidValue {
                field: "profile_id".to_owned(),
                value: args.profile_id.clone(),
                accepted: vec!["<view profile id>".to_owned()],
                hint: Some(
                    "describe_view targets derived views; pass `describe_profile` for loaded profiles"
                        .to_owned(),
                ),
            }
            .into());
        };
        let transforms = view_of_transforms(session.profile().transforms());
        let scope = view_of_scope(session.view_scope());
        let stats = session
            .view_stats()
            .cloned()
            .unwrap_or_else(ViewStats::empty);
        Ok(Json(DescribeViewResult {
            profile_id: args.profile_id,
            base_profile_id,
            transforms,
            scope,
            rule_stats: stats.rule_stats,
            total_base_samples: stats.total_base_samples,
        }))
    }

    #[tool(
        description = "Return a markdown cookbook of canonical `hide_modules` / `hide_frames` \
        regex sets for `create_view`. Covers the three families that keep getting rewritten on \
        every Rust profile: `tracing-subscriber` `Layered<…>` walls, `tokio` runtime internals \
        (scheduler, poll loop, timer wheel, mio), and Rust stdlib glue (drop_in_place, format, \
        panic / unwind). \
        Call this before composing a `create_view` invocation, paste the relevant blocks into \
        `hide_modules` / `hide_frames`, then check `rule_stats` to confirm each pattern matched \
        something. The cookbook is documentation, not a curated list of named presets — drop \
        rules that don't help on your profile and add project-specific ones on a stacked view."
    )]
    pub async fn view_presets(&self) -> Result<Json<ViewPresetsResult>, rmcp::ErrorData> {
        Ok(Json(ViewPresetsResult {
            cookbook: include_str!(
                "../../docs/superpowers/specs/2026-05-06-view-presets-cookbook.md"
            ),
        }))
    }
}

fn view_of_scope(filter: &Filter) -> ScopeView {
    ScopeView {
        thread: filter.thread.as_ref().map(|t| match t {
            ThreadFilter::Tid(n) => format!("tid:{n}"),
            ThreadFilter::Name(n) => n.clone(),
        }),
        process: filter.process.as_ref().map(|p| match p {
            ProcessFilter::Pid(p) => format!("pid:{p}"),
            ProcessFilter::Name(n) => n.clone(),
        }),
        time_range: filter.time_range,
    }
}

fn view_of_transforms(t: &Transforms) -> TransformsView {
    TransformsView {
        hide_frames: t.hide_frames.iter().map(matcher_to_string).collect(),
        hide_modules: t.hide_modules.iter().map(matcher_to_string).collect(),
        keep_only_frames: t.keep_only_frames.iter().map(matcher_to_string).collect(),
        keep_only_modules: t.keep_only_modules.iter().map(matcher_to_string).collect(),
        rename: t
            .rename
            .iter()
            .map(|r| RenameView {
                pattern: matcher_to_string(&r.matcher),
                replacement: r.replacement.clone(),
            })
            .collect(),
        collapse_recursion: t.collapse_recursion,
        strip_type_params: t.strip_type_params,
    }
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

fn build_transforms(args: &CreateViewArgs) -> Result<Transforms, ToolError> {
    let hide_frames = args
        .hide_frames
        .iter()
        .map(|s| required_matcher("hide_frames", s))
        .collect::<Result<Vec<_>, _>>()?;
    let hide_modules = args
        .hide_modules
        .iter()
        .map(|s| required_matcher("hide_modules", s))
        .collect::<Result<Vec<_>, _>>()?;
    let keep_only_frames = args
        .keep_only_frames
        .iter()
        .map(|s| required_matcher("keep_only_frames", s))
        .collect::<Result<Vec<_>, _>>()?;
    let keep_only_modules = args
        .keep_only_modules
        .iter()
        .map(|s| required_matcher("keep_only_modules", s))
        .collect::<Result<Vec<_>, _>>()?;
    let rename = args
        .rename
        .iter()
        .map(|raw| parse_rename_rule(raw))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Transforms {
        hide_frames,
        hide_modules,
        keep_only_frames,
        keep_only_modules,
        collapse_recursion: args.collapse_recursion,
        strip_type_params: args.strip_type_params,
        rename,
    })
}

/// Parse a single rename rule. Required form: `re:<pattern> => <replacement>`.
/// The `re:` prefix is mandatory in v1 — substring renames aren't useful
/// enough to justify a second syntax, and forcing the prefix keeps the
/// matcher path identical to every other tool argument that takes a
/// pattern.
fn parse_rename_rule(raw: &str) -> Result<RenameRule, ToolError> {
    let accepted = || vec!["re:<pattern> => <replacement>".to_owned()];
    let Some(rest) = raw.strip_prefix("re:") else {
        return Err(ToolError::InvalidValue {
            field: "rename".to_owned(),
            value: raw.to_owned(),
            accepted: accepted(),
            hint: Some("rename rules must start with `re:`".to_owned()),
        });
    };
    let Some((pattern, replacement)) = rest.split_once(" => ") else {
        return Err(ToolError::InvalidValue {
            field: "rename".to_owned(),
            value: raw.to_owned(),
            accepted: accepted(),
            hint: Some(
                "rename rules must contain ` => ` separating pattern and replacement".to_owned(),
            ),
        });
    };
    // Round-trip through the shared matcher constructor so a malformed
    // regex surfaces the same `invalid_value` shape (with the parser's
    // caret diagnostic in `hint`) every other tool produces.
    let matcher: FunctionMatcher = required_matcher("rename", &format!("re:{pattern}"))?;
    Ok(RenameRule {
        matcher,
        replacement: replacement.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_transforms_identity_when_all_empty() {
        let args = CreateViewArgs {
            profile_id: "p".into(),
            name: None,
            hide_frames: vec![],
            hide_modules: vec![],
            keep_only_frames: vec![],
            keep_only_modules: vec![],
            collapse_recursion: false,
            strip_type_params: false,
            rename: vec![],
            scope: Default::default(),
        };
        let t = build_transforms(&args).unwrap();
        assert!(t.is_identity());
    }

    #[test]
    fn build_transforms_compiles_hide_lists_and_rename() {
        let args = CreateViewArgs {
            profile_id: "p".into(),
            name: None,
            hide_frames: vec!["malloc".into(), "re:^__".into()],
            hide_modules: vec!["libc.so".into()],
            keep_only_frames: vec![],
            keep_only_modules: vec![],
            collapse_recursion: true,
            strip_type_params: false,
            rename: vec!["re:foo => bar".into()],
            scope: Default::default(),
        };
        let t = build_transforms(&args).unwrap();
        assert_eq!(t.hide_frames.len(), 2);
        assert_eq!(t.hide_modules.len(), 1);
        assert!(t.collapse_recursion);
        assert_eq!(t.rename.len(), 1);
        assert_eq!(t.rename[0].replacement, "bar");
    }

    #[test]
    fn rename_without_re_prefix_rejected() {
        let err = parse_rename_rule("foo => bar").unwrap_err();
        match err {
            ToolError::InvalidValue { field, value, .. } => {
                assert_eq!(field, "rename");
                assert_eq!(value, "foo => bar");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn rename_without_separator_rejected() {
        let err = parse_rename_rule("re:foo bar").unwrap_err();
        match err {
            ToolError::InvalidValue { field, hint, .. } => {
                assert_eq!(field, "rename");
                assert!(hint.unwrap().contains(" => "));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn rename_with_invalid_regex_returns_invalid_value() {
        let err = parse_rename_rule("re:[bad => replacement").unwrap_err();
        match err {
            ToolError::InvalidValue { field, .. } => assert_eq!(field, "rename"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn build_transforms_threads_strip_type_params() {
        let args = CreateViewArgs {
            profile_id: "p".into(),
            name: None,
            hide_frames: vec![],
            hide_modules: vec![],
            keep_only_frames: vec![],
            keep_only_modules: vec![],
            collapse_recursion: false,
            strip_type_params: true,
            rename: vec![],
            scope: Default::default(),
        };
        let t = build_transforms(&args).unwrap();
        assert!(t.strip_type_params);
        assert!(!t.is_identity());
    }

    #[test]
    fn build_transforms_compiles_keep_only_lists() {
        let args = CreateViewArgs {
            profile_id: "p".into(),
            name: None,
            hide_frames: vec![],
            hide_modules: vec![],
            keep_only_frames: vec!["materialize".into(), "re:^differential::".into()],
            keep_only_modules: vec!["timely".into()],
            collapse_recursion: false,
            strip_type_params: false,
            rename: vec![],
            scope: Default::default(),
        };
        let t = build_transforms(&args).unwrap();
        assert_eq!(t.keep_only_frames.len(), 2);
        assert_eq!(t.keep_only_modules.len(), 1);
        assert!(!t.is_identity());
    }

    #[test]
    fn keep_only_frames_empty_pattern_rejected() {
        let args = CreateViewArgs {
            profile_id: "p".into(),
            name: None,
            hide_frames: vec![],
            hide_modules: vec![],
            keep_only_frames: vec!["".into()],
            keep_only_modules: vec![],
            collapse_recursion: false,
            strip_type_params: false,
            rename: vec![],
            scope: Default::default(),
        };
        let err = build_transforms(&args).unwrap_err();
        match err {
            ToolError::InvalidValue { field, .. } => assert_eq!(field, "keep_only_frames"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn hide_frames_empty_pattern_rejected() {
        let args = CreateViewArgs {
            profile_id: "p".into(),
            name: None,
            hide_frames: vec!["".into()],
            hide_modules: vec![],
            keep_only_frames: vec![],
            keep_only_modules: vec![],
            collapse_recursion: false,
            strip_type_params: false,
            rename: vec![],
            scope: Default::default(),
        };
        let err = build_transforms(&args).unwrap_err();
        match err {
            ToolError::InvalidValue { field, .. } => assert_eq!(field, "hide_frames"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }
}
