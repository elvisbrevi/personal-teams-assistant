//! The one command that joins a computer to the account (`machine_join_command`): it downloads the
//! release's install script from this package's repository, installs the prebuilt archive and
//! starts the browser sign-in against the coordinator's public HTTPS origin. The command carries
//! no secret and no maintainer hostname: the origin is the portal's own (or, on a connected
//! computer, its coordinator's) and the repository is the package's (`CARGO_PKG_REPOSITORY`).
use super::*;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Only characters an origin may hold, so the command needs no quoting in any shell.
fn plain(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:/-_".contains(&b))
}

/// The commands for `origin`, validated as `machine_login` validates a coordinator.
pub(in crate::app) fn commands(origin: &str) -> Result<Value> {
    let origin = fleet::endpoint(origin)?.origin().ascii_serialization();
    let repository = REPOSITORY.trim_end_matches('/');
    ensure!(
        repository.starts_with("https://github.com/") && plain(repository) && plain(&origin),
        "This build does not name a release repository for the install command."
    );
    let base = format!("{repository}/releases/latest/download");
    let unix = format!(
        "curl --proto '=https' --tlsv1.2 -fsSL {base}/install.sh | sh -s -- --origin {origin}"
    );
    // Saved to a file and run with a process-only execution policy: no administrator rights, and
    // the script runs byte for byte as the release published it.
    let script = r"$env:TEMP\pta-install.ps1";
    let windows = format!(
        "[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072; Invoke-RestMethod -UseBasicParsing {base}/install.ps1 -OutFile \"{script}\"; powershell -NoProfile -ExecutionPolicy Bypass -File \"{script}\" -Origin {origin}"
    );
    Ok(json!({
        "origin": origin,
        "repository": repository,
        "commands": {"macos": unix, "linux": unix, "windows": windows},
        "uninstall": {
            "macos": format!("curl --proto '=https' --tlsv1.2 -fsSL {base}/install.sh | sh -s -- --uninstall"),
            "linux": format!("curl --proto '=https' --tlsv1.2 -fsSL {base}/install.sh | sh -s -- --uninstall"),
            "windows": format!("Invoke-RestMethod -UseBasicParsing {base}/install.ps1 -OutFile \"{script}\"; powershell -NoProfile -ExecutionPolicy Bypass -File \"{script}\" -Uninstall"),
        },
        "integrations": "Add --integrations (-Integrations on Windows) to also install the skill and register pta mcp with the coding CLIs found; without it the script prints those commands.",
        "trust": "The command holds no secret. The computer appears for approval only after the sign-in page opens on it; Trust this machine there is the only approval, and it grants no directories, Teams ownership, assistant start or provider login.",
    }))
}

/// On a computer connected to a coordinator, the command that joins another computer to it.
pub(super) async fn local(host: &Host) -> Result<Value> {
    let connection = host.state.implementation.connection()?.context(
        "This computer is not connected to a coordinator. Open Settings › Machines in your portal to get the command.",
    )?;
    commands(&connection.url)
}

#[cfg(test)]
mod tests {
    use super::commands;

    #[test]
    fn the_command_names_only_the_origin_and_the_packages_own_release() {
        let shown = commands("https://assistant.example.com/").unwrap();
        assert_eq!(shown["origin"], "https://assistant.example.com");
        let base = format!(
            "{}/releases/latest/download",
            env!("CARGO_PKG_REPOSITORY").trim_end_matches('/')
        );
        let unix = shown["commands"]["linux"].as_str().unwrap();
        assert_eq!(
            unix,
            format!(
                "curl --proto '=https' --tlsv1.2 -fsSL {base}/install.sh | sh -s -- --origin https://assistant.example.com"
            )
        );
        assert_eq!(shown["commands"]["macos"], unix);
        let windows = shown["commands"]["windows"].as_str().unwrap();
        assert!(windows.contains(&format!("{base}/install.ps1")));
        assert!(windows.ends_with("-Origin https://assistant.example.com"));
        assert!(windows.contains("-ExecutionPolicy Bypass -File"));
        // Nothing else reaches the command: no token, cookie or account.
        for command in [unix, windows] {
            assert!(!command.contains("token") && !command.contains("secret"));
        }
        let port = commands("https://assistant.example.com:8443").unwrap();
        assert_eq!(port["origin"], "https://assistant.example.com:8443");
    }

    #[test]
    fn only_a_plain_https_origin_is_accepted() {
        for origin in [
            "http://assistant.example.com",
            "https://user:pass@assistant.example.com",
            "https://assistant.example.com/path",
            "https://assistant.example.com/?q=1",
            "https://assistant.example.com/#x",
            "assistant.example.com",
            "",
        ] {
            assert!(commands(origin).is_err(), "{origin}");
        }
        // A host name that would need quoting cannot be an origin (the URL parser refuses it).
        assert!(commands("https://exa mple.com").is_err());
    }
}
