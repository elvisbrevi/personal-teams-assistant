use crate::app::control::Reply;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Written into every copy this application installs: the version and the SHA-256 of each file.
/// Only a directory whose files still match it is the application's to refresh.
const MANIFEST: &str = ".pta-skill.json";

#[derive(Serialize, Deserialize)]
struct Installed {
    schema: u32,
    version: String,
    files: BTreeMap<String, String>,
}

fn hash(text: &[u8]) -> String {
    format!("{:x}", Sha256::digest(text))
}

fn current() -> Installed {
    Installed {
        schema: 1,
        version: env!("CARGO_PKG_VERSION").into(),
        files: FILES
            .iter()
            .map(|(name, text)| ((*name).to_owned(), hash(text.as_bytes())))
            .collect(),
    }
}
const FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../../desktop/skills/personal-teams-assistant/SKILL.md"),
    ),
    (
        "references/tools-and-todo.md",
        include_str!("../../desktop/skills/personal-teams-assistant/references/tools-and-todo.md"),
    ),
    (
        "references/web-access.md",
        include_str!("../../desktop/skills/personal-teams-assistant/references/web-access.md"),
    ),
    (
        "references/activity-registration.md",
        include_str!(
            "../../desktop/skills/personal-teams-assistant/references/activity-registration.md"
        ),
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
        "references/mcp.md",
        include_str!("../../desktop/skills/personal-teams-assistant/references/mcp.md"),
    ),
    (
        "references/variables.md",
        include_str!("../../desktop/skills/personal-teams-assistant/references/variables.md"),
    ),
    (
        "references/account-configuration.md",
        include_str!(
            "../../desktop/skills/personal-teams-assistant/references/account-configuration.md"
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
        fs::write(target.join(MANIFEST), serde_json::to_string(&current())?)?;
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
            // Remember the copy, so an update can refresh it while it is still unmodified.
            if let Ok(profile) = crate::app::control::profile_dir() {
                let _ = register(&profile, path);
            }
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

// ---------------------------------------------------------------------------------------------
// Copies installed with `pta skill install`, refreshed by an update.

fn registry(profile: &Path) -> PathBuf {
    profile.join("skills").join("registry.json")
}

fn registered(profile: &Path) -> Vec<PathBuf> {
    fs::read_to_string(registry(profile))
        .ok()
        .and_then(|text| serde_json::from_str::<BTreeMap<String, Vec<PathBuf>>>(&text).ok())
        .and_then(|mut all| all.remove("copies"))
        .unwrap_or_default()
}

fn store(profile: &Path, copies: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(profile.join("skills"))?;
    let text = serde_json::to_string(&json!({ "copies": copies }))?;
    let temp = registry(profile).with_extension("tmp");
    fs::write(&temp, text)?;
    crate::security::protect_file(&temp)?;
    crate::security::replace_private_file(&temp, &registry(profile))
}

fn register(profile: &Path, path: &Path) -> Result<()> {
    let mut copies = registered(profile);
    if !copies.iter().any(|c| c == path) {
        copies.push(path.to_owned());
    }
    store(profile, &copies)
}

fn files_below(dir: &Path, base: &Path, found: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            files_below(&path, base, found);
        } else if let Ok(relative) = path.strip_prefix(base) {
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// Whether `dir` is still exactly what this application installed: every recorded file with its
/// recorded content and nothing else.
fn unmodified(dir: &Path) -> Option<Installed> {
    let installed: Installed =
        serde_json::from_str(&fs::read_to_string(dir.join(MANIFEST)).ok()?).ok()?;
    let mut found = Vec::new();
    files_below(dir, dir, &mut found);
    found.retain(|name| name != MANIFEST);
    found.sort();
    let recorded: Vec<&String> = installed.files.keys().collect();
    let same = found.iter().eq(recorded.iter().copied())
        && installed.files.iter().all(|(name, expected)| {
            fs::read(dir.join(name)).is_ok_and(|bytes| &hash(&bytes) == expected)
        });
    same.then_some(installed)
}

fn rewrite(dir: &Path, old: &Installed) -> Result<()> {
    let now = current();
    for (name, content) in FILES {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = dir.join(format!(".{}.refresh", hash(name.as_bytes())));
        fs::write(&temp, content)?;
        crate::security::replace_private_file(&temp, &path)?;
    }
    for name in old.files.keys().filter(|n| !now.files.contains_key(*n)) {
        let _ = fs::remove_file(dir.join(name));
    }
    let temp = dir.join(".manifest.refresh");
    fs::write(&temp, serde_json::to_string(&now)?)?;
    crate::security::replace_private_file(&temp, &dir.join(MANIFEST))
}

/// Brings the application's own installed copies up to this version and says what it did not
/// touch. A copy the user changed, or that is not recorded, is never overwritten. The returned
/// notes are for the person: what was refreshed, what to do about the rest, and that agents which
/// keep `pta mcp` running must reconnect.
pub fn refresh(profile: &Path) -> Vec<String> {
    let mut refreshed = 0usize;
    let mut custom: Vec<String> = Vec::new();
    let mut kept: Vec<PathBuf> = Vec::new();
    for dir in registered(profile) {
        if !dir.is_dir() {
            continue;
        }
        kept.push(dir.clone());
        match unmodified(&dir) {
            Some(old) if old.version == env!("CARGO_PKG_VERSION") => {}
            Some(old) => match rewrite(&dir, &old) {
                Ok(()) => refreshed += 1,
                Err(_) => custom.push(dir.display().to_string()),
            },
            None => custom.push(dir.display().to_string()),
        }
    }
    let _ = store(profile, &kept);
    let mut notes = Vec::new();
    if refreshed > 0 {
        notes.push(format!(
            "Refreshed {refreshed} installed skill {} to version {}.",
            if refreshed == 1 { "copy" } else { "copies" },
            env!("CARGO_PKG_VERSION")
        ));
    }
    if !custom.is_empty() {
        notes.push(format!(
            "Not changed because you modified them or they could not be written: {}. Install the new skill into a new folder with `pta skill install DIRECTORY` and merge your changes.",
            custom.join(", ")
        ));
    }
    notes.push("Skill folders you copied by hand are not tracked: install again with `pta skill install DIRECTORY` if you have any.".into());
    notes.push("Agents that keep `pta mcp` running still use the previous tool catalog: reconnect or restart their MCP server so they read version ".to_owned() + env!("CARGO_PKG_VERSION") + ".");
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unmodified_registered_copies_are_refreshed() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("profile");
        let clean = tmp.path().join("clean");
        let edited = tmp.path().join("edited");
        let extra = tmp.path().join("extra");
        let unknown = tmp.path().join("unknown");
        for dir in [&clean, &edited, &extra] {
            install(dir).unwrap();
            register(&profile, dir).unwrap();
        }
        // An older version of each copy.
        for dir in [&clean, &edited, &extra] {
            let mut old: Installed =
                serde_json::from_str(&fs::read_to_string(dir.join(MANIFEST)).unwrap()).unwrap();
            old.version = "0.0.1".into();
            fs::write(dir.join(MANIFEST), serde_json::to_string(&old).unwrap()).unwrap();
        }
        fs::write(edited.join("SKILL.md"), "my own changes").unwrap();
        fs::write(extra.join("notes.md"), "mine").unwrap();
        // A copy nobody registered, and one whose folder disappeared.
        install(&unknown).unwrap();
        register(&profile, &tmp.path().join("gone")).unwrap();
        let notes = refresh(&profile);
        assert_eq!(
            fs::read_to_string(edited.join("SKILL.md")).unwrap(),
            "my own changes"
        );
        assert_eq!(fs::read_to_string(extra.join("notes.md")).unwrap(), "mine");
        let manifest: Installed =
            serde_json::from_str(&fs::read_to_string(clean.join(MANIFEST)).unwrap()).unwrap();
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            fs::read_to_string(clean.join("SKILL.md")).unwrap(),
            FILES[0].1
        );
        assert!(
            notes[0].starts_with("Refreshed 1 installed skill copy"),
            "{notes:?}"
        );
        let note = notes.iter().find(|n| n.starts_with("Not changed")).unwrap();
        assert!(
            note.contains("edited") && note.contains("extra") && !note.contains("clean"),
            "{note}"
        );
        assert!(notes.iter().any(|n| n.contains("pta mcp")), "{notes:?}");
        // The vanished folder is forgotten; the others stay registered.
        let kept = registered(&profile);
        assert_eq!(kept.len(), 3, "{kept:?}");
        // Refreshing again changes nothing.
        let again = refresh(&profile);
        assert!(!again[0].starts_with("Refreshed"), "{again:?}");
    }
}
