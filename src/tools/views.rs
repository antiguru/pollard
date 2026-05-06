//! `create_view` MCP tool: derive a transformed lazy view of a profile.

use crate::error::ToolError;
use crate::matching::{FunctionMatcher, required_matcher};
use crate::profile::symbolicate::problematic_outcomes;
use crate::profile::transforms::{RenameRule, Transforms};
use crate::query::describe::{DEFAULT_TOP_N, ProfileDescription, describe};
use crate::tools::PollardServer;
use crate::tools::lifecycle::EvictedRef;
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
    /// When true, runs of consecutive same-symbol frames collapse to one
    /// frame in every aggregation.
    #[serde(default)]
    pub collapse_recursion: bool,
    /// Function-name rename rules. Each entry must be `re:<pattern> => <replacement>`.
    /// The `re:` prefix is mandatory: substring renames aren't useful enough to
    /// justify a second syntax in v1.
    #[serde(default)]
    pub rename: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct CreateViewResult {
    pub profile_id: String,
    pub description: ProfileDescription,
    /// Profiles that were evicted from the in-memory cache to make room
    /// for this view. Empty when no eviction was needed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evicted: Vec<EvictedRef>,
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
        Ok(Json(CreateViewResult {
            profile_id: id,
            description: desc,
            evicted,
        }))
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
    let rename = args
        .rename
        .iter()
        .map(|raw| parse_rename_rule(raw))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Transforms {
        hide_frames,
        hide_modules,
        collapse_recursion: args.collapse_recursion,
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
            collapse_recursion: false,
            rename: vec![],
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
            collapse_recursion: true,
            rename: vec!["re:foo => bar".into()],
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
        assert!(matches!(err, ToolError::InvalidValue { .. }));
    }

    #[test]
    fn hide_frames_empty_pattern_rejected() {
        let args = CreateViewArgs {
            profile_id: "p".into(),
            name: None,
            hide_frames: vec!["".into()],
            hide_modules: vec![],
            collapse_recursion: false,
            rename: vec![],
        };
        let err = build_transforms(&args).unwrap_err();
        match err {
            ToolError::InvalidValue { field, .. } => assert_eq!(field, "hide_frames"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }
}
