use pollard::tools::PollardServer;
use rmcp::{ServiceExt, transport::stdio};

const HELP: &str = "\
pollard — MCP server that exposes Firefox-format performance profiles to AI clients.

USAGE:
    pollard              Run the MCP server on stdio (default).
    pollard --version    Print version and exit.
    pollard --help       Print this help and exit.

ENV:
    POLLARD_MAX_PROFILES  Max in-memory profiles before LRU eviction (default: 4).
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    if let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            other => {
                eprintln!("pollard: unknown argument: {other}");
                eprintln!("try `pollard --help`");
                std::process::exit(2);
            }
        }
    }
    run_server()
}

#[tokio::main]
async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let capacity: usize = std::env::var("POLLARD_MAX_PROFILES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);

    let server = PollardServer::new(capacity);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
