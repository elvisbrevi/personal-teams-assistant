use crate::app::control::Reply;
use anyhow::{Result, ensure};
use serde_json::json;
use std::{fs, io::Write, path::Path};
const FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../../desktop/skills/personal-teams-assistant/SKILL.md"),
    ),
    (
        "references/configuration.md",
        include_str!("../../desktop/skills/personal-teams-assistant/references/configuration.md"),
    ),
    (
        "references/lifecycle.md",
        include_str!("../../desktop/skills/personal-teams-assistant/references/lifecycle.md"),
    ),
    (
        "references/auth-and-knowledge.md",
        include_str!(
            "../../desktop/skills/personal-teams-assistant/references/auth-and-knowledge.md"
        ),
    ),
    (
        "references/tests-and-diagnostics.md",
        include_str!(
            "../../desktop/skills/personal-teams-assistant/references/tests-and-diagnostics.md"
        ),
    ),
    (
        "references/azure-wiki.md",
        include_str!("../../desktop/skills/personal-teams-assistant/references/azure-wiki.md"),
    ),
];
fn install(target: &Path) -> Result<()> {
    ensure!(
        target.is_absolute() && !target.exists(),
        "destination must be an unused absolute directory"
    );
    fs::create_dir(target)?;
    let result = (|| -> Result<()> {
        for (name, content) in FILES {
            let path = target.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(content.as_bytes())?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(target);
    }
    result
}
pub fn command(action: &str, destination: Option<&str>) -> Result<Reply> {
    Ok(match action {
        "show" => Reply::success(
            json!({"name":"personal-teams-assistant","version":env!("CARGO_PKG_VERSION"),"content":FILES[0].1}),
        ),
        "install" => {
            let path =
                Path::new(destination.ok_or_else(|| anyhow::anyhow!("missing destination"))?);
            install(path)?;
            Reply::success(json!({"path":path,"version":env!("CARGO_PKG_VERSION")}))
        }
        "path" => {
            let parent = crate::app::control::profile_dir()?.join("skills");
            fs::create_dir_all(&parent)?;
            let path = parent.join(format!(
                "personal-teams-assistant-{}",
                env!("CARGO_PKG_VERSION")
            ));
            if !path.exists() {
                install(&path)?;
            } else {
                for (name, text) in FILES {
                    ensure!(
                        fs::read_to_string(path.join(name))? == *text,
                        "skill cache differs from installed version"
                    );
                }
            }
            Reply::success(json!({"path":path.join("SKILL.md")}))
        }
        _ => anyhow::bail!("unknown skill action"),
    })
}
