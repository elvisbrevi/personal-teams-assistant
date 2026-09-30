#[tokio::main]
async fn main() {
    std::process::exit(personal_teams_desktop::cli::run().await);
}
