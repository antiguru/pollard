//! End-to-end check that view transforms propagate to every aggregator.
//!
//! Uses the `linear_chain` fixture (`a -> b -> c -> d`, all 100 samples landing
//! on the leaf `d`) so hiding `d` is observable: its self-time must
//! re-attribute to its caller `c` once the view is queried via the same
//! aggregator any MCP caller would hit.

use pollard::matching::FunctionMatcher;
use pollard::profile::raw::RawProfile;
use pollard::profile::{Profile, Transforms};
use pollard::query::top_functions::{Args, top_functions};

#[test]
fn hidden_leaf_re_attributes_self_time_to_caller() {
    let raw: RawProfile = serde_json::from_str(include_str!("fixtures/linear_chain.json")).unwrap();
    let base = Profile::from_raw(raw);

    // Baseline sanity: the leaf `d` owns measurable self-time, otherwise the
    // re-attribution check below would be vacuous.
    let baseline = top_functions(&base, &Args::default()).unwrap();
    let leaf_self = baseline
        .functions
        .iter()
        .find(|f| f.function == "d")
        .map(|f| f.self_samples)
        .unwrap_or(0);
    assert!(
        leaf_self > 0,
        "fixture should give `d` measurable self-time, got functions={:?}",
        baseline.functions
    );

    // Build a view that hides the leaf. Substring matcher is what the
    // public `create_view` path produces for a bare pattern.
    let mut t = Transforms::default();
    t.hide_frames.push(FunctionMatcher::new("d").unwrap());
    let view = Profile::view(&base, t);

    let result = top_functions(&view, &Args::default()).unwrap();
    assert!(
        result.functions.iter().all(|f| f.function != "d"),
        "hidden frame must not appear in view aggregation: {:?}",
        result.functions
    );
    let next = result
        .functions
        .iter()
        .find(|f| f.function == "c")
        .expect("caller of `d` should be visible after hiding the leaf");
    assert!(
        next.self_samples >= leaf_self,
        "caller's self-time should absorb the hidden leaf's samples \
         (got self_samples={}, expected >= {leaf_self})",
        next.self_samples,
    );
}
