mod error;
mod matching;
mod profile;
mod query;
mod registry;
mod session;
mod tools;

use rmcp::{ServiceExt, transport::stdio};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let capacity: usize = std::env::var("POLLARD_MAX_PROFILES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);

    let server = tools::PollardServer::new(capacity);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
