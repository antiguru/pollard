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
    /// When true, repeating adjacent cycles in the resolved chain
    /// collapse to one occurrence — `[A, B, C, A, B, C, X]` becomes
    /// `[A, B, C, X]`. Cycles up to length 8 are detected; equality is by
    /// `(function_name, module_name)`. This generalises the simple
    /// "consecutive same-symbol frames" case (cycle length 1) to multi-
    /// function recurrences such as timely's `Subgraph::schedule
    /// → PerOperatorState::schedule → Subgraph::schedule …`.
    pub collapse_recursion: bool,
    /// When true, balanced `<…>` segments are removed from each
    /// frame's `function_name` before `rename` rules fire, so rules
    /// can match the normalized name. `OrdValBatch<RowRowLayout<((Row,
    /// Row), Ts, i64)>>` becomes `OrdValBatch`. Nested brackets are
    /// handled by a depth counter — regex can't match balanced
    /// delimiters reliably.
    pub strip_type_params: bool,
    /// Rename rules applied to `function_name` after hide filters and
    /// `strip_type_params`, and before recursion collapse.
    pub rename: Vec<RenameRule>,
}

#[derive(Debug, Clone)]
pub struct RenameRule {
    /// Compiled matcher that decides whether a frame is renamed.
    pub matcher: FunctionMatcher,
    /// Replacement string passed to [`regex::Regex::replace`] when
    /// `matcher` is a regex, so `$1` / `${name}` interpolate the
    /// capture groups; a literal `$` must be written `$$`. For
    /// substring matchers (not currently producible through the tool
    /// surface) the replacement is used verbatim — the matched frame
    /// is overwritten with this string in full.
    pub replacement: String,
}

impl Transforms {
    pub fn is_identity(&self) -> bool {
        self.hide_frames.is_empty()
            && self.hide_modules.is_empty()
            && !self.collapse_recursion
            && !self.strip_type_params
            && self.rename.is_empty()
    }

    /// Append `other` after `self` to compose two transform layers.
    /// `hide_*` lists union (a frame matching any layer's hide rule is
    /// dropped). `rename` rules concatenate in `[self, other]` order
    /// and fire sequentially during `apply_transforms`, so an `other`
    /// rule sees the result of any matching `self` rule.
    /// `collapse_recursion` and `strip_type_params` are the logical OR.
    pub fn extend_from(&mut self, other: Transforms) {
        self.hide_frames.extend(other.hide_frames);
        self.hide_modules.extend(other.hide_modules);
        self.rename.extend(other.rename);
        self.collapse_recursion |= other.collapse_recursion;
        self.strip_type_params |= other.strip_type_params;
    }
}

/// Remove balanced `<…>` segments from `name`. Walks left-to-right with
/// a depth counter so nested generics drop in one pass; regex can't
/// match balanced delimiters reliably. Unmatched `>` outside any open
/// bracket is preserved verbatim — leaving the character intact is
/// safer than discarding part of a name we couldn't parse.
pub fn strip_type_params_from(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth: usize = 0;
    for c in name.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity() {
        assert!(Transforms::default().is_identity());
    }

    #[test]
    fn strip_type_params_handles_nested_brackets() {
        assert_eq!(
            strip_type_params_from("OrdValBatch<RowRowLayout<((Row, Row), Ts, i64)>>"),
            "OrdValBatch"
        );
        assert_eq!(
            strip_type_params_from("<alloc::vec::Vec<T> as core::ops::Drop>::drop"),
            "::drop"
        );
        assert_eq!(strip_type_params_from("plain_name"), "plain_name");
        assert_eq!(strip_type_params_from(""), "");
    }

    #[test]
    fn strip_type_params_preserves_unbalanced_close() {
        // Unmatched `>` is kept verbatim — discarding it would corrupt
        // names we failed to parse rather than improve them.
        assert_eq!(strip_type_params_from("a>b"), "a>b");
    }

    #[test]
    fn strip_type_params_flag_breaks_identity() {
        let t = Transforms {
            strip_type_params: true,
            ..Default::default()
        };
        assert!(!t.is_identity());
    }

    #[test]
    fn extend_from_ors_strip_type_params() {
        let mut a = Transforms::default();
        let b = Transforms {
            strip_type_params: true,
            ..Default::default()
        };
        a.extend_from(b);
        assert!(a.strip_type_params);
    }
}
