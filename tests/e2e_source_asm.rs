//! End-to-end: build + record + load + source_for_function.
//!
//! This test requires:
//!   - `cc` on PATH (Xcode CLT on macOS or gcc/clang on Linux)
//!   - `samply` on PATH (or at /Users/moritz/.cargo/bin/samply)
//!   - The `POLLARD_E2E` env var set (so build.rs compiles the fixture binary)
//!   - Feature flag `e2e` enabled: `cargo test --features e2e`
//!
//! Run locally:
//!   POLLARD_E2E=1 cargo test --features e2e -- --include-ignored
//!
//! On macOS, samply may need kernel permissions to record. If `samply record`
//! fails due to permissions, the test panics with a clear message and relies
//! on Linux CI for actual coverage.

#[tokio::test]
#[cfg_attr(not(feature = "e2e"), ignore)]
async fn source_for_inner_loop() {
    // Resolve samply — prefer the known install path, fall back to PATH.
    let samply_bin = if std::path::Path::new("/Users/moritz/.cargo/bin/samply").exists() {
        "/Users/moritz/.cargo/bin/samply".to_owned()
    } else {
        "samply".to_owned()
    };

    // The build.rs script compiles target/tiny_program when POLLARD_E2E is set.
    // Verify the binary exists before attempting to record.
    assert!(
        std::path::Path::new("target/tiny_program").exists(),
        "target/tiny_program not found — make sure POLLARD_E2E=1 is set \
         so build.rs compiles it (run: POLLARD_E2E=1 cargo test --features e2e -- --include-ignored)"
    );

    let out = tempfile::NamedTempFile::with_suffix(".json.gz").unwrap();

    let status = std::process::Command::new(&samply_bin)
        .args(["record", "--save-only", "-o"])
        .arg(out.path())
        .arg("--")
        .arg("target/tiny_program")
        .status()
        .unwrap_or_else(|e| panic!("failed to launch samply ({}): {}", samply_bin, e));

    assert!(
        status.success(),
        "samply record exited with status {}. On macOS this can fail if the profiler \
         kernel extension or Rosetta access is not granted. \
         This test passes in Linux CI where ad-hoc profiling is unrestricted.",
        status
    );

    let registry = pollard::registry::SessionRegistry::new(1);
    let id = registry.load(out.path(), None).await.unwrap();
    let session = registry.get(&id).await.unwrap();

    // source_for_function is SYNCHRONOUS — no .await.
    let listing = pollard::query::source::source_for_function(
        session.profile(),
        &pollard::query::source::Args {
            function: "inner_loop".into(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| {
        panic!(
            "source_for_function failed: {:?}\n\
             This may happen if samply does not embed absolute source paths in \
             the profile on this platform. Linux CI typically resolves absolute \
             paths correctly.",
            e
        )
    });

    // At least one line inside inner_loop should have nonzero samples.
    assert!(
        listing.lines.iter().any(|l| l.samples > 0),
        "no samples attributed to any line in inner_loop: {:?}",
        listing.lines
    );
}
