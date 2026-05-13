//! Same three-stage pipeline as `pipeline.rs`, but with the stage functions
//! marked `#[inline(always)]` instead of `#[inline(never)]`.
//!
//! The two binaries do identical work and produce identical checksums; they
//! differ only in whether `route_slow` / `digest_slow` / `rollup_slow` keep
//! their own symbols in the compiled output. In `pipeline`, they do — a
//! profile shows three named hot functions. In `pipeline_inlined`, they
//! collapse into `main`, and leaf-frame attribution has to fall back to
//! source-line or inline-chain reasoning to surface the distinct defects.
//!
//! This is the scenario where pollard's `source_for_function`,
//! `expand_inlines`, and `top_groups` are designed to help: when the
//! function-name axis is too coarse to separate the hotspots.
//!
//! Run:  `cargo run --profile demo --bin pipeline_inlined -- slow`
//!       `cargo run --profile demo --bin pipeline_inlined -- fast`

use std::time::Instant;

const N_EVENTS: usize = 12_000_000;
const N_HOSTS: u32 = 8_000;
const N_METRICS: u8 = 16;
const WINDOW: usize = 20_000;
const GRID_ROWS: usize = 2_048;
const GRID_COLS: usize = 4_096;
const ROLLUP_PASSES: usize = 6;

#[derive(Clone, Copy)]
struct Event {
    host: u32,
    metric: u8,
    value: u32,
}

fn generate_events(n: usize) -> Vec<Event> {
    let mut rng: u64 = 0xfeed_dead_cafe_babe;
    (0..n)
        .map(|_| {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let r = (rng >> 33) as u32;
            Event {
                host: r % N_HOSTS,
                metric: ((r >> 16) as u8) % N_METRICS,
                value: r,
            }
        })
        .collect()
}

fn generate_grid(rows: usize, cols: usize) -> Vec<u32> {
    let mut rng: u64 = 0x1234_5678_9abc_def0;
    (0..rows * cols)
        .map(|_| {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as u32
        })
        .collect()
}

// --- Stage A — slow: format-allocated key, then byte-sum it ---
#[inline(always)]
fn route_slow(events: &[Event]) -> u64 {
    let mut acc: u64 = 0;
    for e in events {
        let key = format!("{}:{}", e.host, e.metric);
        for b in key.bytes() {
            acc = acc.wrapping_add(b as u64);
        }
    }
    acc
}

// --- Stage A — fast: byte-sum the decimal expansion without allocating ---
#[inline(always)]
fn route_fast(events: &[Event]) -> u64 {
    let mut acc: u64 = 0;
    for e in events {
        acc = acc.wrapping_add(digit_byte_sum(e.host));
        acc = acc.wrapping_add(b':' as u64);
        acc = acc.wrapping_add(digit_byte_sum(e.metric as u32));
    }
    acc
}

#[inline(always)]
fn digit_byte_sum(mut n: u32) -> u64 {
    if n == 0 {
        return b'0' as u64;
    }
    let mut s: u64 = 0;
    while n > 0 {
        s = s.wrapping_add((b'0' as u64) + (n % 10) as u64);
        n /= 10;
    }
    s
}

// --- Stage B — slow: a fresh full sort for each percentile ---
#[inline(always)]
fn digest_slow(values: &[u32]) -> u64 {
    let mut acc: u64 = 0;
    for chunk in values.chunks(WINDOW) {
        for &pct in &[1usize, 50, 99] {
            let mut sorted: Vec<u32> = chunk.to_vec();
            sorted.sort_unstable();
            let idx = sorted.len() * pct / 100;
            acc = acc.wrapping_add(sorted[idx] as u64);
        }
    }
    acc
}

// --- Stage B — fast: one partial-sort buffer reused across percentiles ---
#[inline(always)]
fn digest_fast(values: &[u32]) -> u64 {
    let mut acc: u64 = 0;
    for chunk in values.chunks(WINDOW) {
        let mut buf: Vec<u32> = chunk.to_vec();
        let n = buf.len();
        for &pct in &[1usize, 50, 99] {
            let idx = n * pct / 100;
            let (_, v, _) = buf.select_nth_unstable(idx);
            acc = acc.wrapping_add(*v as u64);
        }
    }
    acc
}

// --- Stage C — slow: column-major sum (stride GRID_COLS * 4 bytes) ---
#[inline(always)]
fn rollup_slow(grid: &[u32]) -> u64 {
    let mut acc: u64 = 0;
    for _ in 0..ROLLUP_PASSES {
        for c in 0..GRID_COLS {
            for r in 0..GRID_ROWS {
                acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
            }
        }
    }
    acc
}

// --- Stage C — fast: row-major sum (unit stride) ---
#[inline(always)]
fn rollup_fast(grid: &[u32]) -> u64 {
    let mut acc: u64 = 0;
    for _ in 0..ROLLUP_PASSES {
        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                acc = acc.wrapping_add(grid[r * GRID_COLS + c] as u64);
            }
        }
    }
    acc
}

#[inline(always)]
fn run_slow(events: &[Event], grid: &[u32]) -> u64 {
    let values: Vec<u32> = events.iter().map(|e| e.value).collect();
    let a = route_slow(events);
    let b = digest_slow(&values);
    let c = rollup_slow(grid);
    a.wrapping_add(b).wrapping_add(c)
}

#[inline(always)]
fn run_fast(events: &[Event], grid: &[u32]) -> u64 {
    let values: Vec<u32> = events.iter().map(|e| e.value).collect();
    let a = route_fast(events);
    let b = digest_fast(&values);
    let c = rollup_fast(grid);
    a.wrapping_add(b).wrapping_add(c)
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "slow".into());
    let events = generate_events(N_EVENTS);
    let grid = generate_grid(GRID_ROWS, GRID_COLS);
    let t = Instant::now();
    let checksum = match mode.as_str() {
        "slow" => run_slow(&events, &grid),
        "fast" => run_fast(&events, &grid),
        m => {
            eprintln!("unknown mode: {m} (expected `slow` or `fast`)");
            std::process::exit(2);
        }
    };
    println!(
        "mode={mode}  events={N_EVENTS}  grid={GRID_ROWS}x{GRID_COLS}  elapsed={:?}  checksum=0x{checksum:016x}",
        t.elapsed()
    );
}
