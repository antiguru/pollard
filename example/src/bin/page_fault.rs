//! Per-iteration scratch buffer that exercises the page-fault path.
//!
//! Slow mode `mmap`s a fresh 16 MiB scratch buffer per iteration and walks
//! it once at page stride. Every page is faulted in on first touch; the
//! buffer is `munmap`ped before the next iteration, so the kernel faults
//! the same number of pages again next time around.
//!
//! Fast mode `mmap`s the buffer once outside the loop. The first iteration
//! pays for the faults; every subsequent iteration hits warm pages.
//!
//! The page-fault count differs by ~ITERS×: that's the signal the
//! `page-faults` software event picks up. Wall-clock differs less than the
//! event count because fresh anonymous pages are cheap, but the profile
//! sampled on `page-faults` puts `run_slow` at the top with a much sharper
//! attribution than a cycles profile.
//!
//! Record with `perf` on the `page-faults` software event, then convert to
//! a Firefox-format profile with `samply import`:
//!
//!   perf record -e page-faults --call-graph dwarf -o slow.perf.data \
//!     ./target/demo/page_fault slow
//!   samply import slow.perf.data -o slow.json.gz --save-only
//!
//! Then in pollard:
//!
//!   load_profile  slow.json.gz
//!   top_functions profile="slow" limit=5
//!
//! Run:  `cargo run --profile demo --bin page_fault -- slow`
//!       `cargo run --profile demo --bin page_fault -- fast`

use std::ffi::c_void;
use std::ptr;
use std::time::Instant;

const BUF_BYTES: usize = 16 * 1024 * 1024; // 16 MiB per scratch buffer
const ITERS: usize = 256;
const PAGE: usize = 4096;

/// Anonymous private mmap region. Bypasses the userspace allocator so each
/// `Scratch::new` is guaranteed to map fresh, zero-filled pages from the
/// kernel rather than reusing a cached chunk.
struct Scratch {
    ptr: *mut u8,
    len: usize,
}

impl Scratch {
    fn new(len: usize) -> Self {
        // SAFETY: `mmap` with `MAP_ANONYMOUS | MAP_PRIVATE`, null hint,
        // `fd = -1` and `offset = 0` is the documented anonymous-mapping
        // form. The returned pointer is either a valid mapping of `len`
        // bytes or `MAP_FAILED`, which we check immediately.
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(ptr != libc::MAP_FAILED, "mmap failed");
        Self {
            ptr: ptr.cast(),
            len,
        }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` was returned by `mmap` for exactly `len` bytes of
        // readable/writable memory; the `&mut self` borrow prevents any
        // overlapping reference for the lifetime of the returned slice.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` were returned by `mmap` and have not been
        // unmapped elsewhere; `Scratch` owns the mapping.
        unsafe {
            libc::munmap(self.ptr.cast::<c_void>(), self.len);
        }
    }
}

// --- slide-sized core: slow (fresh mmap per iteration) ---
fn run_slow() -> u64 {
    let mut acc: u64 = 0;
    for iter in 0..ITERS {
        let mut scratch = Scratch::new(BUF_BYTES);
        acc = acc.wrapping_add(touch(scratch.as_mut_slice(), iter));
    }
    acc
}

// --- slide-sized core: fast (one mmap, reused) ---
fn run_fast() -> u64 {
    let mut scratch = Scratch::new(BUF_BYTES);
    let mut acc: u64 = 0;
    for iter in 0..ITERS {
        acc = acc.wrapping_add(touch(scratch.as_mut_slice(), iter));
    }
    acc
}

/// Write a per-iteration marker into the first byte of every page and
/// accumulate it back. Reads only what was just written, so the accumulator
/// is independent of whatever data the buffer carried in.
fn touch(buf: &mut [u8], iter: usize) -> u64 {
    let mark = (iter & 0xff) as u8;
    let mut sum: u64 = 0;
    let mut i = 0;
    while i < buf.len() {
        buf[i] = mark;
        sum = sum.wrapping_add(buf[i] as u64);
        i += PAGE;
    }
    sum
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "slow".into());
    let t = Instant::now();
    let acc = match mode.as_str() {
        "slow" => run_slow(),
        "fast" => run_fast(),
        m => {
            eprintln!("unknown mode: {m} (expected `slow` or `fast`)");
            std::process::exit(2);
        }
    };
    let dt = t.elapsed();
    let pages_per_iter = BUF_BYTES / PAGE;
    println!(
        "mode={mode}  iters={ITERS}  buf={BUF_BYTES}B  pages/iter={pages_per_iter}  elapsed={dt:?}  acc=0x{acc:016x}"
    );
}
