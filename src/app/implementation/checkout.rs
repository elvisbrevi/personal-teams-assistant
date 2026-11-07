//! `checkout_inspect`, a one-shot call on an execution machine: whether a directory inside its
//! allowed roots is a Git checkout, and its `origin` URL. Read-only and confined: it resolves
//! links, reads the checkout's `.git` and its repository `config` (it never runs Git), and every
//! path it reads must resolve inside the roots, so a symbolic link cannot reach outside them.
//! The answer is one of fixed states; nothing of the machine's files but the origin URL, without
//! credentials, leaves it.
use super::paths;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_CONFIG: u64 = 256 * 1024;
const MAX_ORIGIN: usize = 2048;

/// The canonical path, when it resolves inside one of the roots.
fn confined(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    roots
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|root| canonical.starts_with(root))
        .then_some(canonical)
}

fn read_bounded(path: &Path) -> Option<String> {
    let mut text = String::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_CONFIG)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// The `url` of `[remote "origin"]` in a Git config file.
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let header = line.trim_start_matches('[').trim_end_matches(']').trim();
            in_origin = header
                .split_once(char::is_whitespace)
                .is_some_and(|(section, name)| {
                    section.eq_ignore_ascii_case("remote") && name.trim() == "\"origin\""
                });
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            let value = value.trim().trim_matches('"').trim();
            return (!value.is_empty()).then(|| value.to_owned());
        }
    }
    None
}

/// An origin as it may leave the machine: bounded, and without the user and password an HTTPS
/// remote can carry (a personal access token is often written there).
fn shown_origin(origin: &str) -> Option<String> {
    if origin.len() > MAX_ORIGIN || origin.chars().any(char::is_control) {
        return None;
    }
    if let Ok(mut url) = url::Url::parse(origin)
        && matches!(url.scheme(), "https" | "http")
    {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        return Some(url.to_string());
    }
    Some(origin.to_owned())
}

/// The repository config of a checkout whose `.git` is `git`, when it resolves inside the roots.
fn repository_config(directory: &Path, git: &Path, roots: &[PathBuf]) -> Option<String> {
    let metadata = fs::symlink_metadata(git).ok()?;
    let git_dir = if metadata.is_file() {
        // A worktree or submodule: `gitdir: PATH`, relative to the checkout.
        let text = read_bounded(git)?;
        let target = text.trim().strip_prefix("gitdir:")?.trim().to_owned();
        directory.join(target)
    } else {
        git.to_path_buf()
    };
    let git_dir = confined(&git_dir, roots)?;
    let common = match read_bounded(&git_dir.join("commondir")) {
        Some(common) => confined(&git_dir.join(common.trim()), roots)?,
        None => git_dir,
    };
    read_bounded(&confined(&common.join("config"), roots)?)
}

/// At most this many directories are inspected in one call (a workflow's repositories).
pub(super) const MAX_PATHS: usize = crate::workflows::MAX_REPOSITORIES;

/// Inspect every path of one `checkout_inspect` call, in order.
pub(super) fn inspect_all(paths: &Value, roots: &[PathBuf]) -> anyhow::Result<Value> {
    let paths = paths
        .as_array()
        .filter(|p| p.len() <= MAX_PATHS)
        .ok_or_else(|| anyhow::anyhow!("Give at most {MAX_PATHS} directories to inspect."))?;
    let answers: Vec<Value> = paths.iter().map(|p| inspect(p.as_str(), roots)).collect();
    Ok(json!({"checkouts":answers}))
}

/// Inspect `path` on this machine, confined to `roots`. A path that is not, as written, a root
/// or below one is `outside` before anything on disk is read, so the answer never says whether
/// a directory elsewhere exists.
pub(super) fn inspect(path: Option<&str>, roots: &[PathBuf]) -> Value {
    let os = std::env::consts::OS;
    let Some(path) = path.filter(|p| crate::workflows::absolute_on(p, os)) else {
        return json!({"state":"outside"});
    };
    // The roots as saved and as they resolve: both are spellings of the same directories.
    let mut spellings: Vec<String> = Vec::new();
    for root in roots {
        spellings.push(root.to_string_lossy().into_owned());
        if let Ok(canonical) = root.canonicalize() {
            spellings.push(canonical.to_string_lossy().into_owned());
        }
    }
    let Some(root) = spellings
        .iter()
        .find(|root| crate::workflows::under(path, root, os))
    else {
        return json!({"state":"outside"});
    };
    let given = Path::new(path);
    // A symbolic link between the root and the directory makes the path an alias.
    let link = given.ancestors().any(|ancestor| {
        let text = ancestor.to_string_lossy();
        crate::workflows::under(&text, root, os)
            && !crate::workflows::under(root, &text, os)
            && fs::symlink_metadata(ancestor).is_ok_and(|m| m.file_type().is_symlink())
    });
    let Ok(canonical) = given.canonicalize() else {
        return json!({"state":"missing"});
    };
    if confined(&canonical, roots).is_none() {
        return json!({"state":"outside"});
    }
    if !canonical.is_dir() {
        return json!({"state":"missing"});
    }
    let directory = paths::plain(&canonical);
    let git = canonical.join(".git");
    if fs::symlink_metadata(&git).is_err() {
        return json!({"state":"not_checkout","directory":directory,"link":link});
    }
    let origin = repository_config(&canonical, &git, roots)
        .as_deref()
        .and_then(origin_url)
        .as_deref()
        .and_then(shown_origin);
    json!({"state":"checkout","directory":directory,"link":link,"origin":origin})
}

/// The coordinator's reading of a machine's answer. The directory is the machine's own
/// spelling (a Windows machine's `C:\…` on a Linux coordinator too).
pub(in crate::app) fn inspection(answer: &Value) -> crate::workflows::Inspection {
    use crate::workflows::Inspection;
    let directory = answer["directory"]
        .as_str()
        .filter(|d| crate::workflows::absolute(d))
        .map(PathBuf::from);
    let origin = answer["origin"].as_str().and_then(shown_origin);
    let link = answer["link"] == true;
    match (answer["state"].as_str(), directory) {
        (Some("checkout"), Some(directory)) => Inspection::Answered {
            directory,
            link,
            checkout: true,
            origin,
        },
        (Some("not_checkout"), Some(directory)) => Inspection::Answered {
            directory,
            link,
            checkout: false,
            origin: None,
        },
        (Some("outside"), _) => Inspection::Outside,
        (Some("missing"), _) => Inspection::Missing,
        _ => Inspection::Unknown("the machine's answer could not be read".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(dir: &Path, origin: &str) {
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(
            dir.join(".git/config"),
            format!(
                "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.test/other\n[remote \"origin\"]\n\turl = {origin}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn a_checkout_inside_the_roots_reports_its_origin_without_credentials() {
        let root = tempfile::tempdir().unwrap();
        let roots = vec![root.path().to_path_buf()];
        let repo = root.path().join("payments-api");
        checkout(
            &repo,
            "https://user:synthetic-token@dev.azure.com/example/Payments/_git/payments-api",
        );
        let answer = inspect(repo.to_str(), &roots);
        assert_eq!(answer["state"], "checkout");
        assert_eq!(
            answer["origin"],
            "https://dev.azure.com/example/Payments/_git/payments-api"
        );
        assert!(!answer.to_string().contains("synthetic-token"));
        assert_eq!(
            PathBuf::from(answer["directory"].as_str().unwrap()),
            paths::plain(&repo.canonicalize().unwrap())
        );
        let plain = root.path().join("plain");
        fs::create_dir(&plain).unwrap();
        assert_eq!(inspect(plain.to_str(), &roots)["state"], "not_checkout");
        assert_eq!(
            inspect(root.path().join("missing").to_str(), &roots)["state"],
            "missing"
        );
        assert_eq!(inspect(Some("relative"), &roots)["state"], "outside");
        // A worktree: its `.git` file points into the main checkout's Git directory.
        let worktree = root.path().join("feature");
        fs::create_dir_all(repo.join(".git/worktrees/feature")).unwrap();
        fs::write(repo.join(".git/worktrees/feature/commondir"), "../..\n").unwrap();
        fs::create_dir(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                repo.join(".git/worktrees/feature").display()
            ),
        )
        .unwrap();
        assert_eq!(
            inspect(worktree.to_str(), &roots)["origin"],
            "https://dev.azure.com/example/Payments/_git/payments-api"
        );
        assert!(matches!(
            inspection(&inspect(repo.to_str(), &roots)),
            crate::workflows::Inspection::Answered {
                checkout: true,
                origin: Some(_),
                ..
            }
        ));
        // A Windows machine's answer reads the same on any coordinator.
        assert!(matches!(
            inspection(
                &json!({"state":"checkout","directory":r"C:\Work\payments-api","origin":null})
            ),
            crate::workflows::Inspection::Answered { checkout: true, .. }
        ));
        assert_eq!(
            inspection(&json!({"state":"something else"})),
            crate::workflows::Inspection::Unknown("the machine's answer could not be read".into())
        );
    }

    #[test]
    fn nothing_outside_the_roots_is_inspected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = vec![root.path().to_path_buf()];
        checkout(
            outside.path(),
            "https://dev.azure.com/example/Payments/_git/secret",
        );
        assert_eq!(inspect(outside.path().to_str(), &roots)["state"], "outside");
        #[cfg(unix)]
        {
            // A link inside the roots that escapes them.
            let escape = root.path().join("escape");
            std::os::unix::fs::symlink(outside.path(), &escape).unwrap();
            let answer = inspect(escape.to_str(), &roots);
            assert_eq!(answer["state"], "outside");
            assert!(!answer.to_string().contains("secret"));
            // A checkout whose Git directory is a link outside the roots: no origin is read.
            let repo = root.path().join("linked");
            fs::create_dir(&repo).unwrap();
            std::os::unix::fs::symlink(outside.path().join(".git"), repo.join(".git")).unwrap();
            let answer = inspect(repo.to_str(), &roots);
            assert_eq!(answer["state"], "checkout");
            assert!(answer["origin"].is_null());
            // An alias inside the roots reports the canonical directory it names.
            let real = root.path().join("real");
            checkout(
                &real,
                "git@ssh.dev.azure.com:v3/example/Payments/payments-api",
            );
            std::os::unix::fs::symlink(&real, root.path().join("alias")).unwrap();
            let answer = inspect(root.path().join("alias").to_str(), &roots);
            assert_eq!(
                PathBuf::from(answer["directory"].as_str().unwrap()),
                real.canonicalize().unwrap()
            );
            assert_eq!(answer["link"], true);
            assert_eq!(inspect(real.to_str(), &roots)["link"], false);
            assert!(matches!(
                inspection(&answer),
                crate::workflows::Inspection::Answered { link: true, .. }
            ));
        }
        // Whether a directory outside the roots exists is never told: both are `outside`.
        let existing = outside.path().join("exists");
        fs::create_dir(&existing).unwrap();
        assert_eq!(inspect(existing.to_str(), &roots)["state"], "outside");
        assert_eq!(
            inspect(outside.path().join("missing").to_str(), &roots)["state"],
            "outside"
        );
        assert_eq!(
            inspect(root.path().join("../escape").to_str(), &roots)["state"],
            "outside"
        );
        // One call inspects every checkout of a workflow, in order, and no more.
        let answer = inspect_all(
            &json!([
                root.path().join("real"),
                outside.path(),
                root.path().join("none")
            ]),
            &roots,
        )
        .unwrap();
        let states: Vec<&str> = answer["checkouts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["state"].as_str().unwrap())
            .collect();
        assert_eq!(states.len(), 3);
        assert_eq!(&states[1..], ["outside", "missing"]);
        let many: Vec<String> = (0..=MAX_PATHS).map(|n| format!("/x/{n}")).collect();
        assert!(inspect_all(&json!(many), &roots).is_err());
        assert!(inspect_all(&json!("/x"), &roots).is_err());
        assert_eq!(inspect(root.path().to_str(), &[])["state"], "outside");
    }
}
