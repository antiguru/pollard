//! One perf.data recording imported three ways must analyze the same.
//!
//! `multi_v75.json.gz` and `multi_v75.jslb.gz` come from the same
//! samply build, so every query result must match exactly.
//! `multi_v49.json.gz` comes from samply 0.13.1, which emits different
//! libraries and categories, so only per-event totals are compared.
//!
//! `multi_period.json.gz` is a `--weight-by-period` import of the same
//! recording, so its item counts match the others and its weights exceed them.

use std::path::Path;

use pollard::profile::symbolicate::symbolicate;
use pollard::profile::{Profile, load_from_path};
use pollard::query::call_tree::{self, call_tree};
use pollard::query::event::EventSource;
use pollard::query::top_functions::{self, top_functions};

const EVENTS: [&str; 3] = ["cache-misses", "instructions", "branch-misses"];

async fn load(name: &str) -> Profile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/perf")
        .join(name);
    let mut raw = load_from_path(&path).unwrap();
    symbolicate(&mut raw).await.unwrap();
    Profile::from_raw(raw)
}

fn event_sources() -> Vec<EventSource> {
    std::iter::once(EventSource::Samples)
        .chain(EVENTS.iter().map(|e| EventSource::Marker((*e).into())))
        .collect()
}

fn top(p: &Profile, event: EventSource) -> serde_json::Value {
    let out = top_functions(
        p,
        &top_functions::Args {
            event,
            limit: 50,
            ..Default::default()
        },
    )
    .unwrap();
    serde_json::to_value(&out.functions).unwrap()
}

fn total(p: &Profile, event: EventSource) -> u64 {
    top_functions(
        p,
        &top_functions::Args {
            event,
            ..Default::default()
        },
    )
    .unwrap()
    .total_samples
}

#[tokio::test]
async fn json_and_jslb_from_one_build_match() {
    let json = load("multi_v75.json.gz").await;
    let jslb = load("multi_v75.jslb.gz").await;
    for ev in event_sources() {
        assert_eq!(
            top(&json, ev.clone()),
            top(&jslb, ev.clone()),
            "top_functions {ev:?}"
        );
    }
    let tree = |p: &Profile| {
        serde_json::to_value(call_tree(p, &call_tree::Args::default()).unwrap()).unwrap()
    };
    assert_eq!(tree(&json), tree(&jslb));
}

#[tokio::test]
async fn period_json_and_jslb_from_one_build_match() {
    let json = load("multi_period.json.gz").await;
    let jslb = load("multi_period.jslb.gz").await;
    for ev in event_sources() {
        assert_eq!(
            top(&json, ev.clone()),
            top(&jslb, ev.clone()),
            "top_functions {ev:?}"
        );
        let weighted = |p: &Profile| {
            top_functions(
                p,
                &top_functions::Args {
                    event: ev.clone(),
                    ..Default::default()
                },
            )
            .unwrap()
            .weighted
        };
        assert_eq!(weighted(&json), weighted(&jslb), "weighted {ev:?}");
    }
}

#[tokio::test]
async fn versions_49_and_75_agree_on_event_totals() {
    let v49 = load("multi_v49.json.gz").await;
    let v75 = load("multi_v75.json.gz").await;
    for ev in event_sources() {
        let (a, b) = (total(&v49, ev.clone()), total(&v75, ev.clone()));
        assert!(a > 0, "no {ev:?} in the version 49 fixture");
        assert_eq!(a, b, "total for {ev:?}");
    }
}

#[tokio::test]
async fn period_fixture_is_weighted_by_period() {
    let period = load("multi_period.json.gz").await;
    let v75 = load("multi_v75.json.gz").await;

    let events = pollard::query::event::list_events(&period);
    assert_eq!(events[0].event.as_deref(), Some("cycles"));
    for e in &events {
        assert_eq!(e.sampling.as_deref(), Some("frequency 999 Hz"), "{e:?}");
    }
    let plain = pollard::query::event::list_events(&v75);
    for (a, b) in events.iter().zip(plain.iter()) {
        assert_eq!((&a.name, a.count), (&b.name, b.count));
    }

    for ev in event_sources() {
        let out = top_functions(
            &period,
            &top_functions::Args {
                event: ev.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.weighted, "{ev:?}");
        assert!(
            out.total_samples > total(&v75, ev.clone()),
            "period weights must exceed counts for {ev:?}"
        );
    }
}
