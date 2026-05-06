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
