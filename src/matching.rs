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

/// Up to [`NEAREST_K`] candidate function names ranked by fuzzy similarity to
/// the matcher's pattern. The score combines:
///
/// * **Substring containment** (highest tier): if a candidate contains the
///   needle (or vice versa) as a literal substring, it ranks above any
///   non-containing candidate. This preserves the obvious case
///   (`Vec::push` → `<alloc::vec::Vec<T>>::push`).
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
                strsim::sorensen_dice(&needle_lc, &name_lc)
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
    result.into_iter().map(|(_, n)| n).collect()
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
