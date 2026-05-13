//! Inner join between an event stream and a metadata table.
//!
//! Slow mode does a nested-loop scan: O(events * metadata).
//! Fast mode builds a HashMap index once and probes it per event.
//!
//! Run:  `cargo run --release --bin nested_join -- slow`
//!       `cargo run --release --bin nested_join -- fast`

use std::collections::HashMap;
use std::time::Instant;

const N_EVENTS: usize = 500_000;
const N_META: usize = 50_000;

#[derive(Clone, Copy)]
struct Event {
    id: u32,
    value: u32,
}

#[derive(Clone, Copy)]
struct Meta {
    id: u32,
    weight: u32,
}

fn gen_events(n: usize, max_id: u32) -> Vec<Event> {
    let mut rng: u64 = 0xdead_beef_cafe_babe;
    (0..n)
        .map(|_| {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let r = (rng >> 33) as u32;
            Event {
                id: r % max_id,
                value: r,
            }
        })
        .collect()
}

fn gen_meta(n: usize) -> Vec<Meta> {
    (0..n as u32)
        .map(|i| Meta {
            id: i,
            weight: i.wrapping_mul(2654435761),
        })
        .collect()
}

// --- slide-sized core: slow (nested loop) ---
fn join_slow(events: &[Event], meta: &[Meta]) -> u64 {
    let mut acc = 0u64;
    for e in events {
        if let Some(m) = meta.iter().find(|m| m.id == e.id) {
            acc = acc.wrapping_add(u64::from(e.value) ^ u64::from(m.weight));
        }
    }
    acc
}

// --- slide-sized core: fast (hash index) ---
fn join_fast(events: &[Event], meta: &[Meta]) -> u64 {
    let index: HashMap<u32, u32> = meta.iter().map(|m| (m.id, m.weight)).collect();
    let mut acc = 0u64;
    for e in events {
        if let Some(&w) = index.get(&e.id) {
            acc = acc.wrapping_add(u64::from(e.value) ^ u64::from(w));
        }
    }
    acc
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "slow".into());
    let events = gen_events(N_EVENTS, N_META as u32);
    let meta = gen_meta(N_META);

    let t = Instant::now();
    let acc = match mode.as_str() {
        "slow" => join_slow(&events, &meta),
        "fast" => join_fast(&events, &meta),
        m => {
            eprintln!("unknown mode: {m} (expected `slow` or `fast`)");
            std::process::exit(2);
        }
    };
    println!(
        "mode={mode}  events={N_EVENTS}  meta={N_META}  elapsed={:?}  acc=0x{acc:016x}",
        t.elapsed()
    );
}
