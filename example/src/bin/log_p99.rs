//! Per-host p99 latency aggregator over a synthetic log stream.
//!
//! Slow mode has two layered defects:
//!  1. `format!`-ed `String` key per record (allocator pressure, slow hash).
//!  2. Per-emit `clone()` + `sort_unstable` of every bucket
//!     (when only the 99th-percentile element is wanted).
//!
//! Fast mode uses a tuple key and `select_nth_unstable` in place.
//!
//! Run:  `cargo run --release --bin log_p99 -- slow`
//!       `cargo run --release --bin log_p99 -- fast`

use std::collections::HashMap;
use std::time::Instant;

const N_RECORDS: usize = 10_000_000;
const N_HOSTS: u16 = 1_000;
const WINDOW: usize = 100_000;

#[derive(Clone, Copy)]
struct Record {
    host: u16,
    status: u8,
    latency_us: u32,
}

fn generate(n: usize) -> Vec<Record> {
    let mut rng: u64 = 0x1234_5678_9abc_def0;
    (0..n)
        .map(|_| {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let r = (rng >> 33) as u32;
            Record {
                host: (r % u32::from(N_HOSTS)) as u16,
                status: ((r >> 16) & 0x7) as u8,
                latency_us: 100 + (r >> 19) % 50_000,
            }
        })
        .collect()
}

// --- slide-sized core: slow ---
fn run_slow(records: &[Record]) -> u64 {
    let mut buckets: HashMap<String, Vec<u32>> = HashMap::new();
    let mut checksum = 0u64;
    for (i, r) in records.iter().enumerate() {
        let key = format!("{}:{}", r.host, r.status);
        buckets.entry(key).or_default().push(r.latency_us);
        if (i + 1) % WINDOW == 0 {
            for samples in buckets.values() {
                let mut sorted = samples.clone();
                sorted.sort_unstable();
                checksum = checksum.wrapping_add(u64::from(sorted[sorted.len() * 99 / 100]));
            }
        }
    }
    checksum
}

// --- slide-sized core: fast ---
fn run_fast(records: &[Record]) -> u64 {
    let mut buckets: HashMap<(u16, u8), Vec<u32>> =
        HashMap::with_capacity(usize::from(N_HOSTS) * 8);
    let mut checksum = 0u64;
    for (i, r) in records.iter().enumerate() {
        buckets
            .entry((r.host, r.status))
            .or_default()
            .push(r.latency_us);
        if (i + 1) % WINDOW == 0 {
            for samples in buckets.values_mut() {
                let idx = samples.len() * 99 / 100;
                let (_, p99, _) = samples.select_nth_unstable(idx);
                checksum = checksum.wrapping_add(u64::from(*p99));
            }
        }
    }
    checksum
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "slow".into());
    let records = generate(N_RECORDS);
    let t = Instant::now();
    let checksum = match mode.as_str() {
        "slow" => run_slow(&records),
        "fast" => run_fast(&records),
        m => {
            eprintln!("unknown mode: {m} (expected `slow` or `fast`)");
            std::process::exit(2);
        }
    };
    println!(
        "mode={mode}  records={N_RECORDS}  elapsed={:?}  checksum=0x{checksum:016x}",
        t.elapsed()
    );
}
