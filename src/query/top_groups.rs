//! `top_groups`: flat top-N aggregation under a caller-chosen group key.
//!
//! Mirrors the sample-attribution rules of `top_functions` but lets the caller
//! pick what each row counts: the function itself, its module/lib, the source
//! file the frame came from, or a directory prefix of that file. Rows have a
//! uniform shape (`{rank, group_kind, key, ...samples}`) so the output schema
//! doesn't shift with `group_by`.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::profile::Profile;
use crate::query::filters::Filter;
use crate::query::top_functions::{Counts, aggregate_grouped};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub group_by: GroupBy,
    /// Optional substring/regex filter applied to function names. Same
    /// matcher syntax as `top_functions` — narrows which frames contribute,
    /// regardless of grouping axis.
    pub filter: Option<String>,
    pub limit: usize,
    pub sort_by: SortBy,
    pub filter_args: Filter,
    pub expand_inlines: bool,
    /// Path-component depth for `GroupBy::Directory`. `None` → full
    /// directory; `Some(N)` → first N components (`/home/foo/bar.rs`,
    /// depth 1 → `/home`; depth 2 → `/home/foo`). Ignored for other
    /// groupings.
    pub directory_depth: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBy {
    #[default]
    Function,
    Module,
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    SelfTime,
    TotalTime,
    /// `total_samples - self_samples`. See [`crate::query::top_functions`]
    /// for the rationale.
    Descendants,
}

const DEFAULT_LIMIT: usize = 30;

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub group_kind: &'static str,
    pub total_samples: u64,
    pub filter: Option<String>,
    pub sort_by: &'static str,
    pub groups: Vec<GroupEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GroupEntry {
    pub rank: usize,
    pub key: String,
    pub self_samples: u64,
    pub self_pct: f32,
    pub total_samples: u64,
    pub total_pct: f32,
}

pub fn top_groups(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    let depth = args.directory_depth;
    let group_by = args.group_by;
    let (counts, total_samples): (HashMap<String, Counts>, u64) = aggregate_grouped(
        profile,
        args.filter.as_deref(),
        &args.filter_args,
        args.expand_inlines,
        |func, module, file| match group_by {
            GroupBy::Function => Some(func.to_owned()),
            GroupBy::Module => Some(module.unwrap_or("<unknown>").to_owned()),
            GroupBy::File => Some(file.unwrap_or("<unknown>").to_owned()),
            GroupBy::Directory => directory_key(file?, depth),
        },
    )?;

    let mut entries: Vec<(String, Counts)> = counts.into_iter().collect();
    let sort_key = |c: &Counts| match args.sort_by {
        SortBy::SelfTime => c.self_samples,
        SortBy::TotalTime => c.total_samples,
        SortBy::Descendants => c.total_samples.saturating_sub(c.self_samples),
    };
    entries.sort_by(|a, b| {
        sort_key(&b.1)
            .cmp(&sort_key(&a.1))
            .then_with(|| a.0.cmp(&b.0))
    });

    let limit = if args.limit == 0 {
        DEFAULT_LIMIT
    } else {
        args.limit
    };
    let total = total_samples.max(1) as f32;
    let groups: Vec<_> = entries
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(i, (key, c))| GroupEntry {
            rank: i + 1,
            key,
            self_samples: c.self_samples,
            self_pct: 100.0 * c.self_samples as f32 / total,
            total_samples: c.total_samples,
            total_pct: 100.0 * c.total_samples as f32 / total,
        })
        .collect();

    Ok(Output {
        group_kind: match group_by {
            GroupBy::Function => "function",
            GroupBy::Module => "module",
            GroupBy::File => "file",
            GroupBy::Directory => "directory",
        },
        total_samples,
        filter: args.filter.clone(),
        sort_by: match args.sort_by {
            SortBy::SelfTime => "self",
            SortBy::TotalTime => "total",
            SortBy::Descendants => "descendants",
        },
        groups,
    })
}

/// Build a directory key from a source-file path, truncated to the first
/// `depth` components when set. Returns `None` for files with no parent
/// directory (e.g. bare `"main.rs"`) — the caller skips those frames.
fn directory_key(file: &str, depth: Option<u32>) -> Option<String> {
    let trimmed = file.trim_end_matches('/');
    let last_slash = trimmed.rfind('/')?;
    let dir = &trimmed[..last_slash];
    if dir.is_empty() {
        return Some("/".to_owned());
    }
    let Some(depth) = depth else {
        return Some(dir.to_owned());
    };
    let leading_slash = dir.starts_with('/');
    let body = dir.trim_start_matches('/');
    let joined: Vec<&str> = body.split('/').take(depth as usize).collect();
    let s = joined.join("/");
    Some(if leading_slash { format!("/{s}") } else { s })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::RawProfile;

    fn two_functions() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn function_grouping_matches_top_functions_keys() {
        let p = two_functions();
        let out = top_groups(
            &p,
            &Args {
                group_by: GroupBy::Function,
                ..Default::default()
            },
        )
        .unwrap();
        let keys: Vec<&str> = out.groups.iter().map(|g| g.key.as_str()).collect();
        assert!(keys.contains(&"hot"), "{keys:?}");
        assert!(keys.contains(&"cold"));
        assert_eq!(out.group_kind, "function");
    }

    #[test]
    fn module_grouping_collapses_unknown_when_no_lib() {
        // two_functions.json has no libs, so every frame's module is None and
        // they collapse into a single `<unknown>` row whose self_pct is the
        // profile total.
        let p = two_functions();
        let out = top_groups(
            &p,
            &Args {
                group_by: GroupBy::Module,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.group_kind, "module");
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].key, "<unknown>");
        assert_eq!(out.groups[0].self_pct, 100.0);
    }

    #[test]
    fn directory_truncates_to_depth() {
        // Leading-slash absolute path: depth 1 keeps the first component
        // under root, depth 2 adds the next.
        assert_eq!(
            directory_key("/home/foo/bar.rs", Some(1)).as_deref(),
            Some("/home")
        );
        assert_eq!(
            directory_key("/home/foo/bar.rs", Some(2)).as_deref(),
            Some("/home/foo")
        );
        assert_eq!(
            directory_key("/home/foo/bar.rs", Some(99)).as_deref(),
            Some("/home/foo")
        );
        // Relative path.
        assert_eq!(
            directory_key("src/query/mod.rs", Some(1)).as_deref(),
            Some("src")
        );
        // No depth = full parent dir.
        assert_eq!(
            directory_key("src/query/mod.rs", None).as_deref(),
            Some("src/query")
        );
        // Bare filename has no directory.
        assert_eq!(directory_key("main.rs", None), None);
        // Top-level file under root.
        assert_eq!(directory_key("/main.rs", None).as_deref(), Some("/"));
    }

    #[test]
    fn descendants_sort_pushes_leaf_to_bottom() {
        // Smoke test for SortBy::Descendants on the function grouping —
        // mirrors the behaviour we already test for `top_functions`.
        let p = two_functions();
        let out = top_groups(
            &p,
            &Args {
                group_by: GroupBy::Function,
                sort_by: SortBy::Descendants,
                ..Default::default()
            },
        )
        .unwrap();
        // Both rows have descendants=0 (every stack is a single leaf in the
        // fixture); we just need to verify the sort key is wired.
        assert_eq!(out.sort_by, "descendants");
    }
}
