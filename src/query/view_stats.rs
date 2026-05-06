//! Per-rule statistics for a profile view's transforms.
//!
//! Computed once at view-create time so callers can confirm each rule
//! actually fired. Silent zero-match rules typically signal a typo in
//! `hide_frames` / `hide_modules` / `keep_only_frames` /
//! `keep_only_modules` / `rename` — without this counter
//! users can only infer the miss by running downstream tools and
//! noticing nothing changed.

use crate::matching::matcher_to_string;
use crate::profile::Profile;
use crate::profile::event_source::EventSource;
use crate::profile::parsed::{MAX_CYCLE_LEN, ResolvedFrame, collapse_cycles};
use schemars::JsonSchema;
use serde::Serialize;

/// One rule's diagnostic counters.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RuleStat {
    /// Index within the rule's kind list (0-based). For `collapse_recursion`
    /// always 0 — there's only one such rule per composed transform.
    pub rule_index: usize,
    /// `"hide_frames"`, `"hide_modules"`, `"keep_only_frames"`,
    /// `"keep_only_modules"`, `"rename"`, `"strip_type_params"`, or
    /// `"collapse_recursion"`.
    pub kind: String,
    /// User-facing pattern: `re:<regex>` for regex matchers, raw
    /// substring for substring matchers, `<matcher> => <replacement>`
    /// for renames, empty for `collapse_recursion`.
    pub pattern: String,
    /// Total number of frames (across all sampled stacks) the rule
    /// matched. `0` is the typo signal.
    pub frames_matched: u64,
    /// Distinct samples whose stack had at least one frame the rule
    /// affected.
    pub samples_affected: u64,
}

/// Aggregate view stats: per-rule counters plus the underlying base's
/// total sample count, so the share absorbed by each rule is
/// computable as `samples_affected / total_base_samples`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ViewStats {
    pub rule_stats: Vec<RuleStat>,
    /// Total samples in the underlying base profile (all threads,
    /// regardless of whether the sample carried a stack).
    pub total_base_samples: u64,
}

impl ViewStats {
    pub fn empty() -> Self {
        Self {
            rule_stats: Vec::new(),
            total_base_samples: 0,
        }
    }
}

/// Walk every sample in `profile` and tally per-rule hits for the
/// view's composed transforms. Mirrors `Profile::apply_transforms`
/// step-for-step so the counts reflect what aggregators actually see.
///
/// Inline expansion is *off* here — view stats are diagnostic, and
/// every aggregator already reaches the same per-rule answer with or
/// without inlining once the user verifies the pattern matched.
pub fn compute_view_stats(profile: &Profile) -> ViewStats {
    let t = profile.transforms();
    let mut total_base_samples: u64 = 0;
    for thread in profile.threads() {
        total_base_samples += thread.raw().samples.length as u64;
    }
    if t.is_identity() {
        return ViewStats {
            rule_stats: Vec::new(),
            total_base_samples,
        };
    }

    let mut hf_frames: Vec<u64> = vec![0; t.hide_frames.len()];
    let mut hf_samples: Vec<u64> = vec![0; t.hide_frames.len()];
    let mut hm_frames: Vec<u64> = vec![0; t.hide_modules.len()];
    let mut hm_samples: Vec<u64> = vec![0; t.hide_modules.len()];
    let mut kf_frames: Vec<u64> = vec![0; t.keep_only_frames.len()];
    let mut kf_samples: Vec<u64> = vec![0; t.keep_only_frames.len()];
    let mut km_frames: Vec<u64> = vec![0; t.keep_only_modules.len()];
    let mut km_samples: Vec<u64> = vec![0; t.keep_only_modules.len()];
    let mut rn_frames: Vec<u64> = vec![0; t.rename.len()];
    let mut rn_samples: Vec<u64> = vec![0; t.rename.len()];
    let mut cl_frames: u64 = 0;
    let mut cl_samples: u64 = 0;
    let mut sp_frames: u64 = 0;
    let mut sp_samples: u64 = 0;

    for thread in profile.threads() {
        let handle = thread.handle();
        // Materialize the per-thread stack indices first so the chain
        // building below doesn't re-borrow `profile` while iterating.
        let stacks: Vec<Option<usize>> = profile
            .stack_indices(handle, &EventSource::Samples, None)
            .collect();
        for stack_opt in stacks {
            let Some(stack_idx) = stack_opt else { continue };
            // Build root-to-leaf raw chain — same as `resolved_chain`
            // but skipping the transform pass so we can replay each
            // rule and count.
            let leaf_first: Vec<usize> = profile.walk_stack(handle, stack_idx).collect();
            let mut chain: Vec<ResolvedFrame> = Vec::with_capacity(leaf_first.len());
            for &fi in leaf_first.iter().rev() {
                let Some(info) = profile.frame_info(handle, fi) else {
                    continue;
                };
                chain.push(ResolvedFrame {
                    function: info.function_name.to_owned(),
                    module: info.module_name.map(str::to_owned),
                    file: info.file.map(str::to_owned),
                    line: info.line,
                    column: info.column,
                    address: info.address,
                });
            }

            // Keep-only pass. Mirrors `apply_keep_only`: a frame
            // survives iff any rule matches; runs of non-matching
            // frames collapse to a single placeholder. Diagnostics
            // count every matching rule independently for both
            // counters, matching the hide-pass shape so an overlap
            // between two patterns shows up as both rules' work.
            if !t.keep_only_frames.is_empty() || !t.keep_only_modules.is_empty() {
                let mut kf_seen = vec![false; t.keep_only_frames.len()];
                let mut km_seen = vec![false; t.keep_only_modules.len()];
                let mut kept: Vec<ResolvedFrame> = Vec::with_capacity(chain.len());
                let mut in_run = false;
                for f in chain.drain(..) {
                    let mut keep = false;
                    for (i, m) in t.keep_only_frames.iter().enumerate() {
                        if m.matches(&f.function) {
                            kf_frames[i] += 1;
                            kf_seen[i] = true;
                            keep = true;
                        }
                    }
                    if let Some(mm) = f.module.as_deref() {
                        for (i, mp) in t.keep_only_modules.iter().enumerate() {
                            if mp.matches(mm) {
                                km_frames[i] += 1;
                                km_seen[i] = true;
                                keep = true;
                            }
                        }
                    }
                    if keep {
                        kept.push(f);
                        in_run = false;
                    } else if !in_run {
                        kept.push(ResolvedFrame {
                            function: crate::profile::transforms::KEEP_ONLY_PLACEHOLDER
                                .to_owned(),
                            module: None,
                            file: None,
                            line: None,
                            column: None,
                            address: None,
                        });
                        in_run = true;
                    }
                }
                chain = kept;
                for (i, &seen) in kf_seen.iter().enumerate() {
                    if seen {
                        kf_samples[i] += 1;
                    }
                }
                for (i, &seen) in km_seen.iter().enumerate() {
                    if seen {
                        km_samples[i] += 1;
                    }
                }
            }

            // Hide pass. Production semantics: a frame is dropped if
            // *any* hide rule matches. For diagnostics we count every
            // matching rule independently so two overlapping rules
            // each show their work.
            let mut hf_seen = vec![false; t.hide_frames.len()];
            let mut hm_seen = vec![false; t.hide_modules.len()];
            chain.retain(|f| {
                let mut drop = false;
                for (i, m) in t.hide_frames.iter().enumerate() {
                    if m.matches(&f.function) {
                        hf_frames[i] += 1;
                        hf_seen[i] = true;
                        drop = true;
                    }
                }
                if let Some(mm) = f.module.as_deref() {
                    for (i, mp) in t.hide_modules.iter().enumerate() {
                        if mp.matches(mm) {
                            hm_frames[i] += 1;
                            hm_seen[i] = true;
                            drop = true;
                        }
                    }
                }
                !drop
            });
            for (i, &seen) in hf_seen.iter().enumerate() {
                if seen {
                    hf_samples[i] += 1;
                }
            }
            for (i, &seen) in hm_seen.iter().enumerate() {
                if seen {
                    hm_samples[i] += 1;
                }
            }

            // Strip pass. Counts every frame whose name actually
            // changes — names without `<…>` segments are no-ops and
            // don't show up here, so a zero count is the typo signal
            // ("did the option even fire?").
            if t.strip_type_params {
                let mut sp_seen = false;
                for f in chain.iter_mut() {
                    let stripped = crate::profile::transforms::strip_type_params_from(&f.function);
                    if stripped != f.function {
                        sp_frames += 1;
                        sp_seen = true;
                        f.function = stripped;
                    }
                }
                if sp_seen {
                    sp_samples += 1;
                }
            }

            // Rename pass. Sequential: a rule sees the result of any
            // previous rename — same contract as `apply_transforms`.
            let mut rn_seen = vec![false; t.rename.len()];
            for f in chain.iter_mut() {
                for (i, rule) in t.rename.iter().enumerate() {
                    if rule.matcher.matches(&f.function) {
                        rn_frames[i] += 1;
                        rn_seen[i] = true;
                        f.function = rule.matcher.replace(&f.function, &rule.replacement);
                    }
                }
            }
            for (i, &seen) in rn_seen.iter().enumerate() {
                if seen {
                    rn_samples[i] += 1;
                }
            }

            // Collapse pass. `frames_matched` is the count of frames
            // removed; `samples_affected` is whether this sample lost
            // any frames at all.
            if t.collapse_recursion {
                let before = chain.len();
                collapse_cycles(&mut chain, MAX_CYCLE_LEN);
                let removed = before.saturating_sub(chain.len()) as u64;
                if removed > 0 {
                    cl_frames += removed;
                    cl_samples += 1;
                }
            }
        }
    }

    let mut rule_stats: Vec<RuleStat> = Vec::new();
    for (i, m) in t.keep_only_frames.iter().enumerate() {
        rule_stats.push(RuleStat {
            rule_index: i,
            kind: "keep_only_frames".to_owned(),
            pattern: matcher_to_string(m),
            frames_matched: kf_frames[i],
            samples_affected: kf_samples[i],
        });
    }
    for (i, m) in t.keep_only_modules.iter().enumerate() {
        rule_stats.push(RuleStat {
            rule_index: i,
            kind: "keep_only_modules".to_owned(),
            pattern: matcher_to_string(m),
            frames_matched: km_frames[i],
            samples_affected: km_samples[i],
        });
    }
    for (i, m) in t.hide_frames.iter().enumerate() {
        rule_stats.push(RuleStat {
            rule_index: i,
            kind: "hide_frames".to_owned(),
            pattern: matcher_to_string(m),
            frames_matched: hf_frames[i],
            samples_affected: hf_samples[i],
        });
    }
    for (i, m) in t.hide_modules.iter().enumerate() {
        rule_stats.push(RuleStat {
            rule_index: i,
            kind: "hide_modules".to_owned(),
            pattern: matcher_to_string(m),
            frames_matched: hm_frames[i],
            samples_affected: hm_samples[i],
        });
    }
    for (i, rule) in t.rename.iter().enumerate() {
        rule_stats.push(RuleStat {
            rule_index: i,
            kind: "rename".to_owned(),
            pattern: format!(
                "{} => {}",
                matcher_to_string(&rule.matcher),
                rule.replacement
            ),
            frames_matched: rn_frames[i],
            samples_affected: rn_samples[i],
        });
    }
    if t.strip_type_params {
        rule_stats.push(RuleStat {
            rule_index: 0,
            kind: "strip_type_params".to_owned(),
            pattern: String::new(),
            frames_matched: sp_frames,
            samples_affected: sp_samples,
        });
    }
    if t.collapse_recursion {
        rule_stats.push(RuleStat {
            rule_index: 0,
            kind: "collapse_recursion".to_owned(),
            pattern: String::new(),
            frames_matched: cl_frames,
            samples_affected: cl_samples,
        });
    }

    ViewStats {
        rule_stats,
        total_base_samples,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::FunctionMatcher;
    use crate::profile::raw::RawProfile;
    use crate::profile::transforms::{RenameRule, Transforms};

    fn linear_profile() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn identity_view_yields_no_rule_stats() {
        let base = linear_profile();
        let stats = compute_view_stats(&base);
        assert!(stats.rule_stats.is_empty());
        assert!(stats.total_base_samples > 0);
    }

    #[test]
    fn hide_frames_rule_counts_matching_frames_and_samples() {
        let base = linear_profile();
        let view = Profile::view(
            &base,
            Transforms {
                hide_frames: vec![FunctionMatcher::new("d").unwrap()],
                ..Default::default()
            },
        );
        let stats = compute_view_stats(&view);
        let r = stats
            .rule_stats
            .iter()
            .find(|r| r.kind == "hide_frames")
            .unwrap();
        assert_eq!(r.pattern, "d");
        assert!(r.frames_matched > 0, "rule should match at least once");
        assert!(r.samples_affected > 0);
        assert!(r.samples_affected <= stats.total_base_samples);
    }

    #[test]
    fn typo_rule_reports_zero_matches() {
        let base = linear_profile();
        let view = Profile::view(
            &base,
            Transforms {
                hide_frames: vec![FunctionMatcher::new("notarealfunctionname").unwrap()],
                ..Default::default()
            },
        );
        let stats = compute_view_stats(&view);
        let r = stats
            .rule_stats
            .iter()
            .find(|r| r.kind == "hide_frames")
            .unwrap();
        assert_eq!(r.frames_matched, 0);
        assert_eq!(r.samples_affected, 0);
    }

    #[test]
    fn rename_rule_counts_all_renames() {
        let base = linear_profile();
        let view = Profile::view(
            &base,
            Transforms {
                rename: vec![RenameRule {
                    matcher: FunctionMatcher::new("d").unwrap(),
                    replacement: "leaf".to_owned(),
                }],
                ..Default::default()
            },
        );
        let stats = compute_view_stats(&view);
        let r = stats
            .rule_stats
            .iter()
            .find(|r| r.kind == "rename")
            .unwrap();
        assert_eq!(r.pattern, "d => leaf");
        assert!(r.frames_matched > 0);
    }

    #[test]
    fn strip_type_params_emits_stat_when_enabled() {
        let base = linear_profile();
        let view = Profile::view(
            &base,
            Transforms {
                strip_type_params: true,
                ..Default::default()
            },
        );
        let stats = compute_view_stats(&view);
        let count = stats
            .rule_stats
            .iter()
            .filter(|r| r.kind == "strip_type_params")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn keep_only_frames_rule_counts_kept_frames() {
        let base = linear_profile();
        let view = Profile::view(
            &base,
            Transforms {
                keep_only_frames: vec![FunctionMatcher::new("c").unwrap()],
                ..Default::default()
            },
        );
        let stats = compute_view_stats(&view);
        let r = stats
            .rule_stats
            .iter()
            .find(|r| r.kind == "keep_only_frames")
            .unwrap();
        assert_eq!(r.pattern, "c");
        // linear_chain has 100 samples, each with frame c once.
        assert_eq!(r.frames_matched, 100);
        assert_eq!(r.samples_affected, 100);
    }

    #[test]
    fn keep_only_typo_reports_zero_matches() {
        let base = linear_profile();
        let view = Profile::view(
            &base,
            Transforms {
                keep_only_frames: vec![FunctionMatcher::new("notarealframename").unwrap()],
                ..Default::default()
            },
        );
        let stats = compute_view_stats(&view);
        let r = stats
            .rule_stats
            .iter()
            .find(|r| r.kind == "keep_only_frames")
            .unwrap();
        assert_eq!(r.frames_matched, 0);
        assert_eq!(r.samples_affected, 0);
    }

    #[test]
    fn collapse_recursion_emits_single_stat_when_enabled() {
        let base = linear_profile();
        let view = Profile::view(
            &base,
            Transforms {
                collapse_recursion: true,
                ..Default::default()
            },
        );
        let stats = compute_view_stats(&view);
        let count = stats
            .rule_stats
            .iter()
            .filter(|r| r.kind == "collapse_recursion")
            .count();
        assert_eq!(count, 1);
    }
}
