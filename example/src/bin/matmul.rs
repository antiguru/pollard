//! Dense f32 matrix multiply (row-major).
//!
//! Slow mode is the textbook ijk loop. The inner `k` loop strides through
//! `b` one column at a time, missing the cache on every load.
//!
//! Fast mode swaps the inner two loops to ikj: the same arithmetic, but the
//! inner loop now strides through `b` row-major, matching cache lines.
//!
//! Run:  `cargo run --release --bin matmul -- slow`
//!       `cargo run --release --bin matmul -- fast`

use std::time::Instant;

const N: usize = 1280;

// --- slide-sized core: slow (ijk, B column-stride) ---
fn matmul_slow(a: &[f32], b: &[f32], c: &mut [f32]) {
    for i in 0..N {
        for j in 0..N {
            let mut s = 0.0f32;
            for k in 0..N {
                s += a[i * N + k] * b[k * N + j];
            }
            c[i * N + j] = s;
        }
    }
}

// --- slide-sized core: fast (ikj, B row-stride) ---
fn matmul_fast(a: &[f32], b: &[f32], c: &mut [f32]) {
    for i in 0..N {
        for k in 0..N {
            let a_ik = a[i * N + k];
            for j in 0..N {
                c[i * N + j] += a_ik * b[k * N + j];
            }
        }
    }
}

fn fill(seed: u32) -> Vec<f32> {
    let mut rng = seed;
    (0..N * N)
        .map(|_| {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            ((rng >> 8) as f32) / (1u32 << 24) as f32
        })
        .collect()
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "slow".into());
    let a = fill(1);
    let b = fill(2);
    let mut c = vec![0.0f32; N * N];

    let t = Instant::now();
    match mode.as_str() {
        "slow" => matmul_slow(&a, &b, &mut c),
        "fast" => matmul_fast(&a, &b, &mut c),
        m => {
            eprintln!("unknown mode: {m} (expected `slow` or `fast`)");
            std::process::exit(2);
        }
    }
    let dt = t.elapsed();

    let trace: f32 = (0..N).map(|i| c[i * N + i]).sum();
    let gflops = (2.0 * (N as f64).powi(3)) / dt.as_secs_f64() / 1e9;
    println!("mode={mode}  N={N}  elapsed={dt:?}  GFLOPS={gflops:.2}  trace={trace:.4}");
}
