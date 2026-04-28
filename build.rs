use std::process::Command;

fn main() {
    if std::env::var("POLLARD_E2E").is_err() {
        return;
    }
    let status = Command::new("cc")
        .args(["-O1", "-g", "tests/fixtures/tiny_program.c", "-o", "target/tiny_program"])
        .status()
        .expect("cc must be on PATH for e2e tests");
    assert!(status.success());
    println!("cargo:rerun-if-changed=tests/fixtures/tiny_program.c");
}
