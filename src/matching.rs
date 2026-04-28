//! Function name matching: substring by default, regex with `re:` prefix.
//!
//! Used uniformly by every tool parameter that takes a function name
//! (`filter`, `function`, `root_function`, `paths_to`).

#![allow(dead_code)]

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
            Self::Substring(needle) => function_name.contains(needle.as_str()),
            Self::Regex(re) => re.is_match(function_name),
        }
    }
}

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
