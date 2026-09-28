use anyhow::Result;
use personal_teams_assistant::ado;

#[tokio::main]
async fn main() -> Result<()> {
    let key = std::env::var("AZURE_DEVOPS_TOKEN")?;
    let catalog = std::fs::read_to_string("../personal-teams-knowledge/azure-devops.toml")?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let evidence = ado::status(&client, &key, &catalog).await?;
    println!(
        "Azure DevOps read succeeded: {} characters, {} HU references, {} related tasks, {} commits, {} pipelines, {} releases",
        evidence.chars().count(),
        evidence.matches("HU #").count(),
        evidence.matches("Tarea relacionada:").count(),
        evidence.matches("Commit reciente").count(),
        evidence.matches("Pipeline de commit").count(),
        evidence.matches("Release asociado").count()
    );
    Ok(())
}
