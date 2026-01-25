#[tokio::main]
async fn main() {
    if let Err(err) = infra::mcp::server::run_stdio().await {
        eprintln!("infra: {}", err);
        std::process::exit(1);
    }
}
