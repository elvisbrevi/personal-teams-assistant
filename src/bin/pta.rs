#[tokio::main]
async fn main() {
    std::process::exit(personal_teams_assistant::app::cli::run().await);
}
