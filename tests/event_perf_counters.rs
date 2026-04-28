//! Opt-in smoke test against a real samply perf-counters profile.
//!
//! Requires `POLLARD_SMOKE_PROFILE` to point at a multi-event samply
//! profile. Skipped (with a printed note) when unset so CI passes
//! without external fixtures.
//!
//! Usage:
//!   POLLARD_SMOKE_PROFILE=/path/to/perf-counters.json \
//!     cargo test --test event_perf_counters -- --ignored --nocapture

use pollard::profile::Profile;
use pollard::profile::raw::RawProfile;
use pollard::query::event::EventSource;
use pollard::query::top_functions::{Args, top_functions};

#[test]
#[ignore]
fn smoke_perf_counters_real_profile() {
    let path = std::env::var("POLLARD_SMOKE_PROFILE")
        .expect("set POLLARD_SMOKE_PROFILE to run this test");
    let bytes = std::fs::read(&path).expect("read profile");
    let raw: RawProfile = serde_json::from_slice(&bytes).expect("parse profile");
    let p = Profile::from_raw(raw);

    for ev in [
        EventSource::Samples,
        EventSource::Marker("cache-misses".into()),
        EventSource::Marker("branch-misses".into()),
        EventSource::Marker("instructions".into()),
    ] {
        let out = top_functions(
            &p,
            &Args {
                event: ev.clone(),
                limit: 3,
                ..Default::default()
            },
        )
        .unwrap();
        eprintln!(
            "event={:<14} total={:>8} top_self={}",
            out.event,
            out.total_samples,
            out.functions
                .first()
                .map(|f| f.self_samples)
                .unwrap_or(0),
        );
        assert!(
            out.total_samples > 0,
            "expected nonzero total for event {ev:?}; profile may not contain that marker",
        );
    }
}
