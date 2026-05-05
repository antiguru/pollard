//! Measure resident-set-size growth from loading a profile.
//!
//! Usage: `cargo run --release --example measure_rss -- <path-to-profile>`
//!
//! Reports VmRSS before and after `ProfileSession::load`, the delta, and
//! VmHWM (peak RSS over the process lifetime). Linux only — reads
//! `/proc/self/status`. The session is held to end-of-`main` so VmRSS
//! reflects the loaded profile, not a freed one.
//!
//! Numbers here feed the memory-management section of the design doc;
//! the previous "100–500 MB" estimate was unverified.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use pollard::session::ProfileSession;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .ok_or("usage: measure_rss <path-to-profile.json[.gz]>")?
        .into();

    let file_size_bytes = fs::metadata(&path)?.len();

    let before = read_status();
    let t0 = Instant::now();
    let session = ProfileSession::load(&path, None).await?;
    let load_ms = t0.elapsed().as_millis();
    let after = read_status();

    let thread_count = session.profile().threads().count();
    let sample_count: usize = session
        .profile()
        .threads()
        .map(|t| t.raw().samples.stack.len())
        .sum();

    println!("path:                  {}", path.display());
    println!(
        "file size:             {} ({:.1} MiB)",
        file_size_bytes,
        file_size_bytes as f64 / 1024.0 / 1024.0
    );
    println!("load time:             {load_ms} ms");
    println!("threads:               {thread_count}");
    println!("samples (sum):         {sample_count}");
    println!();
    println!("VmRSS before:          {} kB", before.rss_kb);
    println!("VmRSS after:           {} kB", after.rss_kb);
    println!(
        "VmRSS delta:           {} kB ({:.1} MiB)",
        after.rss_kb.saturating_sub(before.rss_kb),
        (after.rss_kb.saturating_sub(before.rss_kb)) as f64 / 1024.0
    );
    println!(
        "VmHWM (peak):          {} kB ({:.1} MiB)",
        after.hwm_kb,
        after.hwm_kb as f64 / 1024.0
    );
    println!(
        "RSS / file-size ratio: {:.1}x",
        after.rss_kb as f64 * 1024.0 / file_size_bytes as f64
    );

    drop(session);
    Ok(())
}

struct Status {
    rss_kb: u64,
    hwm_kb: u64,
}

fn read_status() -> Status {
    let text = fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    let mut rss_kb = 0;
    let mut hwm_kb = 0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("VmHWM:") {
            hwm_kb = parse_kb(rest);
        }
    }
    Status { rss_kb, hwm_kb }
}

fn parse_kb(rest: &str) -> u64 {
    rest.split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
