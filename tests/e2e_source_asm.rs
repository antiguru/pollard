//! End-to-end: build + record + load + source_for_function / asm_for_function.
//!
//! Requires:
//!   - `cc` on PATH
//!   - `samply` on PATH
//!   - `POLLARD_E2E=1` (so build.rs compiles the fixture binary)
//!   - `--features e2e` (so this test runs instead of being ignored)
//!
//! Run locally:
//!   POLLARD_E2E=1 cargo test --features e2e -- --include-ignored

#[tokio::test]
#[cfg_attr(not(feature = "e2e"), ignore)]
async fn source_for_inner_loop() {
    let samply_bin = "samply";

    // The build.rs script compiles target/tiny_program when POLLARD_E2E is set.
    // Verify the binary exists before attempting to record.
    assert!(
        std::path::Path::new("target/tiny_program").exists(),
        "target/tiny_program not found — make sure POLLARD_E2E=1 is set \
         so build.rs compiles it (run: POLLARD_E2E=1 cargo test --features e2e -- --include-ignored)"
    );

    let out = tempfile::NamedTempFile::with_suffix(".json.gz").unwrap();

    let status = std::process::Command::new(samply_bin)
        .args(["record", "--save-only", "-o"])
        .arg(out.path())
        .arg("--")
        .arg("target/tiny_program")
        .status()
        .unwrap_or_else(|e| panic!("failed to launch samply: {}", e));

    assert!(
        status.success(),
        "samply record exited with status {}",
        status
    );

    let registry = pollard::registry::SessionRegistry::new(1);
    let (id, _evicted) = registry.load(out.path(), None).await.unwrap();
    let session = registry.get(&id).await.unwrap();

    // source_for_function is SYNCHRONOUS — no .await.
    let listing = pollard::query::source::source_for_function(
        session.profile(),
        &pollard::query::source::Args {
            function: "inner_loop".into(),
            ..Default::default()
        },
    )
    .unwrap();

    // At least one line inside inner_loop should have nonzero samples.
    assert!(
        listing.lines.iter().any(|l| l.samples > 0),
        "no samples attributed to any line in inner_loop: {:?}",
        listing.lines
    );
}

#[tokio::test]
#[cfg_attr(not(feature = "e2e"), ignore)]
async fn asm_for_inner_loop() {
    let samply_bin = "samply";

    // The build.rs script compiles target/tiny_program when POLLARD_E2E is set.
    assert!(
        std::path::Path::new("target/tiny_program").exists(),
        "target/tiny_program not found — make sure POLLARD_E2E=1 is set \
         so build.rs compiles it (run: POLLARD_E2E=1 cargo test --features e2e -- --include-ignored)"
    );

    let out = tempfile::NamedTempFile::with_suffix(".json.gz").unwrap();

    let status = std::process::Command::new(samply_bin)
        .args(["record", "--save-only", "-o"])
        .arg(out.path())
        .arg("--")
        .arg("target/tiny_program")
        .status()
        .unwrap_or_else(|e| panic!("failed to launch samply: {}", e));

    assert!(
        status.success(),
        "samply record exited with status {}",
        status
    );

    let registry = pollard::registry::SessionRegistry::new(1);
    let (id, _evicted) = registry.load(out.path(), None).await.unwrap();
    let session = registry.get(&id).await.unwrap();

    let listing = pollard::query::asm::asm_for_function(
        session.profile(),
        &pollard::query::asm::Args {
            function: "inner_loop".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(
        !listing.instructions.is_empty(),
        "no instructions returned: {:?}",
        listing
    );
    assert!(
        listing.instructions.iter().any(|i| i.samples > 0),
        "no sample attribution on any instruction: {:?}",
        listing.instructions
    );
}
