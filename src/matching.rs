//! Function name matching: substring by default, regex with `re:` prefix.
//!
//! Used uniformly by every tool parameter that takes a function name
//! (`filter`, `function`, `root_function`, `paths_to`).

#![allow(dead_code)]

use crate::profile::Profile;
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
        // Tolerate HTML-encoded patterns. LLM clients sometimes encode `<` and
        // `>` in generic types (e.g. `&lt;Vec&gt;::push`), and the resulting
        // mismatch against the demangled symbol would otherwise look like a
        // brittle "function not found".
        let decoded = decode_html_entities(pattern);
        if let Some(re) = decoded.strip_prefix("re:") {
            Ok(Self::Regex(Regex::new(re)?))
        } else {
            Ok(Self::Substring(decoded))
        }
    }

    pub fn matches(&self, function_name: &str) -> bool {
        match self {
            Self::Substring(needle) => function_name.contains(needle.as_str()),
            Self::Regex(re) => re.is_match(function_name),
        }
    }
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        // `&amp;` last so we don't double-decode `&amp;lt;` into `<`.
        .replace("&amp;", "&")
}

/// Render a matcher back to its user-facing pattern (regex prefixed with
/// `re:`). Used in error messages and when ranking nearest matches.
pub fn matcher_to_string(matcher: &FunctionMatcher) -> String {
    match matcher {
        FunctionMatcher::Substring(s) => s.clone(),
        FunctionMatcher::Regex(r) => format!("re:{}", r.as_str()),
    }
}

/// Splits an identifier into lowercase tokens for fuzzy matching.
///
/// Splits on `::`, angle brackets, parentheses, brackets, commas,
/// underscores, ampersands, asterisks, dots, quotes, and whitespace, plus
/// camelCase boundaries (`getElement` → `get`, `element`). Empty fragments
/// are dropped.
pub fn tokenize_identifier(s: &str) -> Vec<String> {
    let separators = [
        ':', '<', '>', '(', ')', '[', ']', ',', '_', '&', '*', '.', ' ', '\t', '\'', '"',
    ];
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for ch in s.chars() {
        if separators.contains(&ch) {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower {
            tokens.push(std::mem::take(&mut current));
            current.push(ch);
            prev_lower = false;
            continue;
        }
        current.push(ch);
        prev_lower = ch.is_lowercase();
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens.iter_mut().for_each(|t| *t = t.to_lowercase());
    tokens.retain(|t| !t.is_empty());
    tokens
}

/// Length of the longest in-order subsequence of `needle` tokens that appear
/// (in order, possibly with gaps) inside `cand`. Used to reward candidates
/// that contain every needle token in the right order, even when generic
/// arguments or namespace fragments sit between them.
fn token_lcs_len(needle: &[String], cand: &[String]) -> usize {
    if needle.is_empty() || cand.is_empty() {
        return 0;
    }
    let mut prev = vec![0_usize; cand.len() + 1];
    let mut curr = vec![0_usize; cand.len() + 1];
    for i in 1..=needle.len() {
        for j in 1..=cand.len() {
            curr[j] = if needle[i - 1] == cand[j - 1] {
                prev[j - 1] + 1
            } else {
                curr[j - 1].max(prev[j])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.iter_mut().for_each(|x| *x = 0);
    }
    prev[cand.len()]
}

/// Fraction of `needle` tokens that occur anywhere in `cand` (needle-coverage,
/// not symmetric Jaccard — partial-needle matches still get partial credit).
fn token_set_coverage(needle: &[String], cand: &[String]) -> f64 {
    if needle.is_empty() {
        return 0.0;
    }
    let cand_set: std::collections::HashSet<&str> = cand.iter().map(String::as_str).collect();
    let matched = needle
        .iter()
        .filter(|t| cand_set.contains(t.as_str()))
        .count();
    matched as f64 / needle.len() as f64
}

/// Up to [`NEAREST_K`] candidate function names ranked by fuzzy similarity to
/// the matcher's pattern. The score combines:
///
/// * **Substring containment** (highest tier): if a candidate contains the
///   needle (or vice versa) as a literal substring, it ranks above any
///   non-containing candidate. This preserves the obvious case
///   (`Vec::push` → `<alloc::vec::Vec<T>>::push`).
/// * **Token-aware overlap** (middle tier): tokenizes both sides on `::`,
///   `<>`, `_`, and camelCase boundaries, then rewards candidates that
///   contain every needle token in order. Catches `Vec::push` →
///   `<alloc::vec::Vec<T,A>>::push`, where literal substring fails because
///   the generic arguments interrupt the token sequence.
/// * **Sørensen–Dice bigram overlap** (fallback): rewards shared character
///   pairs regardless of position, so insertions like `cols_third` →
///   `simd_cols_3rd` still surface near the top. Jaro–Winkler was too
///   prefix-biased for this case and let unrelated symbols outrank obvious
///   typos.
///
/// Comparison is case-insensitive. Regex matchers are scored against the
/// regex source text, which is a coarse approximation but better than
/// nothing.
///
/// Memory is bounded to `NEAREST_K` entries — a min-heap streams candidates
/// and evicts the lowest score on overflow, so a profile with millions of
/// distinct symbols still costs the same as one with five.
pub fn nearest_function_names(profile: &Profile, matcher: &FunctionMatcher) -> Vec<String> {
    nearest_function_scored(profile, matcher)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Same ranking as [`nearest_function_names`] but exposes the raw scores so
/// callers can apply confidence thresholds — used by [`auto_promote_match`]
/// to decide whether a single fuzzy hit is high-confidence enough to substitute
/// for an exact lookup.
pub fn nearest_function_scored(profile: &Profile, matcher: &FunctionMatcher) -> Vec<(String, f64)> {
    use std::cmp::{Ordering, Reverse};
    use std::collections::BinaryHeap;

    /// Wrapper that gives `f64` a total order via [`f64::total_cmp`], so it can
    /// live inside `BinaryHeap`. Higher score = better.
    #[derive(Clone, Copy)]
    struct Score(f64);
    impl PartialEq for Score {
        fn eq(&self, other: &Self) -> bool {
            self.0.total_cmp(&other.0).is_eq()
        }
    }
    impl Eq for Score {}
    impl Ord for Score {
        fn cmp(&self, other: &Self) -> Ordering {
            self.0.total_cmp(&other.0)
        }
    }
    impl PartialOrd for Score {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    let needle = matcher_to_string(matcher);
    let needle_lc = needle.to_lowercase();
    let needle_toks = tokenize_identifier(&needle);

    // Min-heap of (score, name) keyed by `Reverse(score)` so the worst
    // candidate sits on top and is the one we pop when the heap exceeds K.
    // The String tie-breaker keeps output deterministic when scores collide.
    let mut heap: BinaryHeap<Reverse<(Score, String)>> = BinaryHeap::with_capacity(NEAREST_K + 1);

    for thread in profile.threads() {
        let raw = thread.raw();
        for func_idx in 0..raw.func_table.length {
            let Some(s_idx) = raw.func_table.name.get(func_idx) else {
                continue;
            };
            let Some(name) = raw.string_array.get(*s_idx) else {
                continue;
            };

            // Cheap dedup: same function name can appear in multiple threads.
            // Heap holds at most K+1 entries, so this scan is bounded.
            if heap.iter().any(|Reverse((_, n))| n == name) {
                continue;
            }

            let name_lc = name.to_lowercase();
            let score = if name_lc.contains(&needle_lc) {
                // Reward containing matches strongly; tie-break toward the
                // closest-sized candidate.
                2.0 - (name.len() as f64 - needle.len() as f64).abs() / 1024.0
            } else if needle_lc.contains(&name_lc) {
                1.5 - (needle.len() as f64 - name.len() as f64).abs() / 1024.0
            } else {
                // Token tier sits between reverse-containment (1.5) and
                // substring (2.0): a full in-order token match scores ~1.9,
                // partial matches blend with the bigram floor.
                let cand_toks = tokenize_identifier(name);
                let token_score = if !needle_toks.is_empty() && !cand_toks.is_empty() {
                    let lcs =
                        token_lcs_len(&needle_toks, &cand_toks) as f64 / needle_toks.len() as f64;
                    let cov = token_set_coverage(&needle_toks, &cand_toks);
                    if (lcs - 1.0).abs() < f64::EPSILON {
                        1.5 + 0.4 * cov
                    } else {
                        0.6 * lcs + 0.4 * cov
                    }
                } else {
                    0.0
                };
                let dice = strsim::sorensen_dice(&needle_lc, &name_lc);
                token_score.max(dice)
            };

            heap.push(Reverse((Score(score), name.clone())));
            if heap.len() > NEAREST_K {
                heap.pop();
            }
        }
    }

    let mut result: Vec<(Score, String)> = heap.into_iter().map(|Reverse(t)| t).collect();
    // Sort highest score first, lexicographic tie-break.
    result.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    result.into_iter().map(|(s, n)| (n, s.0)).collect()
}

/// A high-confidence promotion of a fuzzy match: surfaced to the caller
/// alongside the resolved tool output so they can verify they got the
/// function they meant.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct DidYouMean {
    /// The function name as the caller originally typed it.
    pub needle: String,
    /// The fully-qualified name we matched against and used for the response.
    pub resolved: String,
}

/// Threshold a candidate's score must clear to be promotion-eligible. 1.5 is
/// the bottom of the token tier (full in-order token match) and the top of
/// the reverse-containment tier — anything below is bigram noise.
const PROMOTE_MIN_SCORE: f64 = 1.5;

/// Multiplicative gap the top score must have over a contesting candidate.
/// Only contests against other promotion-eligible scores: if the runner-up
/// is below [`PROMOTE_MIN_SCORE`] we treat the top as uncontested. Tuned
/// conservatively per the issue's 1.5× suggestion.
const PROMOTE_GAP: f64 = 1.5;

/// Examines a scored fuzzy ranking and returns the resolved name when the
/// top candidate is a clear winner. Two conditions must hold:
///
/// * top score clears [`PROMOTE_MIN_SCORE`] (avoids promoting bigram noise);
/// * any other candidate above the same threshold is dominated by at least
///   [`PROMOTE_GAP`]× (avoids picking arbitrarily between two confident hits).
///
/// Returns `None` when the field is too crowded to safely guess. The caller
/// is expected to surface the promotion via [`DidYouMean`] so the user can
/// audit the substitution.
pub fn auto_promote_match(scored: &[(String, f64)]) -> Option<&str> {
    let (top_name, top_score) = scored.first().map(|(n, s)| (n.as_str(), *s))?;
    if top_score < PROMOTE_MIN_SCORE {
        return None;
    }
    for (_, other) in scored.iter().skip(1) {
        // Below-floor candidates can't credibly contest the top — they live
        // in the bigram tier and would otherwise block any single-confident
        // hit just by existing.
        if *other < PROMOTE_MIN_SCORE {
            continue;
        }
        if top_score < PROMOTE_GAP * *other {
            return None;
        }
    }
    Some(top_name)
}

/// Maximum number of suggestions returned by [`nearest_function_names`].
pub const NEAREST_K: usize = 5;

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

    #[test]
    fn html_entities_decoded_in_pattern() {
        let m = FunctionMatcher::new("&lt;Vec&gt;::push").unwrap();
        assert!(m.matches("<Vec>::push"));
        assert_eq!(matcher_to_string(&m), "<Vec>::push");
    }

    #[test]
    fn html_entities_decoded_in_regex_body() {
        let m = FunctionMatcher::new("re:^&lt;.*&gt;$").unwrap();
        assert!(m.matches("<T>"));
    }

    #[test]
    fn nested_amp_does_not_double_decode() {
        // `&amp;lt;` should decode to `&lt;`, not to `<`.
        let m = FunctionMatcher::new("&amp;lt;").unwrap();
        assert_eq!(matcher_to_string(&m), "&lt;");
    }

    use crate::profile::Profile;
    use crate::profile::raw::RawProfile;

    fn profile_with_funcs(names: &[&str]) -> Profile {
        let func_indices: Vec<String> = (0..names.len()).map(|i| i.to_string()).collect();
        let json = format!(
            r#"{{
                "meta": {{"interval": 1.0, "startTime": 0.0, "product": "test"}},
                "libs": [],
                "threads": [{{
                    "name": "Main",
                    "tid": 1,
                    "pid": 1,
                    "registerTime": 0.0,
                    "stringArray": [{names_json}],
                    "frameTable": {{"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []}},
                    "stackTable": {{"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []}},
                    "samples": {{"length": 0, "stack": [], "time": [], "weight": null, "weightType": "samples"}},
                    "funcTable": {{
                        "length": {n},
                        "name": [{idx}],
                        "isJS": [{js}],
                        "relevantForJS": [{js}],
                        "resource": [{rsrc}],
                        "fileName": [{nones}],
                        "lineNumber": [{nones}],
                        "columnNumber": [{nones}]
                    }},
                    "resourceTable": {{"length": 0, "lib": [], "name": [], "host": [], "type": []}},
                    "nativeSymbols": {{"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}}
                }}]
            }}"#,
            n = names.len(),
            names_json = names
                .iter()
                .map(|s| serde_json::to_string(s).unwrap())
                .collect::<Vec<_>>()
                .join(","),
            idx = func_indices.join(","),
            js = vec!["false"; names.len()].join(","),
            rsrc = vec!["-1"; names.len()].join(","),
            nones = vec!["null"; names.len()].join(","),
        );
        let raw: RawProfile = serde_json::from_str(&json).expect("valid profile JSON");
        Profile::from_raw(raw)
    }

    #[test]
    fn nearest_substring_outranks_unrelated() {
        let p = profile_with_funcs(&["alloc::vec::Vec::push", "memcpy", "free", "rustfmt::main"]);
        let matcher = FunctionMatcher::new("Vec::push").unwrap();
        let near = nearest_function_names(&p, &matcher);
        assert_eq!(near[0], "alloc::vec::Vec::push");
    }

    #[test]
    fn nearest_typo_finds_close_neighbor() {
        let p = profile_with_funcs(&["memcpy", "memmove", "memset", "free"]);
        let matcher = FunctionMatcher::new("memcyp").unwrap();
        let near = nearest_function_names(&p, &matcher);
        assert_eq!(near[0], "memcpy");
    }

    #[test]
    fn nearest_outranks_unrelated_on_insertion_typo() {
        // Real-world report: needle `cols_third` should suggest
        // `simd_cols_3rd` over unrelated symbols like `EntryPoint` /
        // `_GI_execve` that share no bigrams with the needle.
        let p = profile_with_funcs(&[
            "EntryPoint",
            "_GI_execve",
            "simd::main",
            "int_realloc",
            "dl_start",
            "simd_cols_3rd",
        ]);
        let matcher = FunctionMatcher::new("cols_third").unwrap();
        let near = nearest_function_names(&p, &matcher);
        assert_eq!(
            near[0], "simd_cols_3rd",
            "expected simd_cols_3rd to outrank unrelated symbols, got {near:?}"
        );
    }

    #[test]
    fn nearest_token_match_beats_unrelated_when_substring_fails() {
        // From issue #20: needle `Vec::push` should suggest the demangled
        // form even when generic arguments break literal substring match.
        let p = profile_with_funcs(&[
            "<alloc::vec::Vec<T,A>>::push",
            "memcpy",
            "free",
            "rustfmt::main",
        ]);
        let matcher = FunctionMatcher::new("Vec::push").unwrap();
        let near = nearest_function_names(&p, &matcher);
        assert_eq!(
            near[0], "<alloc::vec::Vec<T,A>>::push",
            "expected token-aware match to surface generic-laden Vec::push, got {near:?}"
        );
    }

    #[test]
    fn tokenize_splits_on_namespace_and_generics() {
        let toks = tokenize_identifier("<alloc::vec::Vec<T,A>>::push");
        assert_eq!(toks, vec!["alloc", "vec", "vec", "t", "a", "push"]);
    }

    #[test]
    fn tokenize_splits_camelcase() {
        let toks = tokenize_identifier("getElementByName");
        assert_eq!(toks, vec!["get", "element", "by", "name"]);
    }

    #[test]
    fn auto_promote_returns_top_when_clear_winner() {
        let scored = vec![("Vec::push".to_owned(), 1.9), ("memcpy".to_owned(), 0.4)];
        assert_eq!(auto_promote_match(&scored), Some("Vec::push"));
    }

    #[test]
    fn auto_promote_returns_none_when_runner_up_close() {
        // Two candidates within 1.5× — too crowded to safely guess.
        let scored = vec![("Vec::push".to_owned(), 1.6), ("Vec::pop".to_owned(), 1.55)];
        assert_eq!(auto_promote_match(&scored), None);
    }

    #[test]
    fn auto_promote_returns_none_when_top_below_threshold() {
        // Top score in bigram tier — not confident enough to promote.
        let scored = vec![("memcpy".to_owned(), 0.7), ("memmove".to_owned(), 0.5)];
        assert_eq!(auto_promote_match(&scored), None);
    }

    #[test]
    fn auto_promote_returns_top_when_only_one_candidate() {
        let scored = vec![("Vec::push".to_owned(), 1.7)];
        assert_eq!(auto_promote_match(&scored), Some("Vec::push"));
    }

    #[test]
    fn auto_promote_returns_none_for_empty() {
        assert_eq!(auto_promote_match(&[]), None);
    }

    #[test]
    fn nearest_caps_at_five() {
        let names: Vec<String> = (0..20).map(|i| format!("fn_{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let p = profile_with_funcs(&refs);
        let matcher = FunctionMatcher::new("fn_07").unwrap();
        let near = nearest_function_names(&p, &matcher);
        assert_eq!(near.len(), 5);
        assert!(near.contains(&"fn_07".to_owned()));
    }
}
