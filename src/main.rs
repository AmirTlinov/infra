#[tokio::main(flavor = "current_thread")]
async fn main() {
    std::process::exit(infra::cli::run().await);
}
