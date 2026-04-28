//! Partial deserialization of the Firefox processed-profile JSON.
//!
//! We only deserialize the fields we need for v1 tools:
//! - lib table (for symbolication and module names)
//! - func table, frame table, stack table, sample table (for aggregation)
//! - resource table, string array (for name resolution)
//! - thread/process metadata
//!
//! Markers, counters, profiler config, and other top-level fields are skipped.

pub mod load;
pub mod parsed;
pub mod raw;
pub mod symbolicate;

pub use load::load_from_path;
#[allow(unused_imports)]
pub use parsed::{Profile, ProcessHandle, ThreadHandle};
