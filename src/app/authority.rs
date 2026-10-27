//! Authority comes from the owning profile and authenticated transport, never arguments.
use super::*;
use serde_json::Value;

pub(super) const EXECUTION_DENIED: &str = "Shared-host execution requires a verified OS security boundary. Use your personal trusted machine; a working directory or tool-less prompt is not an OS sandbox.";
pub(super) const ACCOUNT_UNAVAILABLE: &str = "This assistant account is unavailable or its authorization changed. Sign in again or contact the portal operator.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountBinding {
    pub id: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    pub profile: PathBuf,
    pub account: Option<AccountBinding>,
    pub owner: bool,
    pub shared: bool,
    pub admitted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transport {
    /// The control endpoint's local token: the CLI, MCP and anything an agent can run.
    Local,
    /// The application's own window, reaching the host in process.
    Window,
    Browser,
    Machine,
}

#[derive(Clone)]
pub(super) struct Invocation {
    pub scope: Scope,
    pub transport: Transport,
}

tokio::task_local! {
    static CURRENT: Invocation;
}

pub(super) async fn within<T>(
    invocation: Invocation,
    operation: impl std::future::Future<Output = T>,
) -> T {
    CURRENT.scope(invocation, operation).await
}

pub(super) fn invocation(state: &DesktopState) -> Result<Invocation> {
    let scope = web::account_scope(state.config_path.parent().unwrap())?;
    ensure!(
        scope == state.authority,
        "Account binding changed. Restart this host."
    );
    let invocation = CURRENT.try_with(Clone::clone).unwrap_or(Invocation {
        scope: scope.clone(),
        transport: Transport::Window,
    });
    ensure!(invocation.scope == scope, "Account binding mismatch.");
    ensure!(
        scope.admitted,
        "This assistant account is unavailable. Contact the portal operator."
    );
    Ok(invocation)
}

pub(super) fn reject_authority_arguments(args: &Value) -> Result<()> {
    for key in [
        "account",
        "account_id",
        "account_version",
        "owner",
        "owner_id",
        "profile",
        "current_profile",
        "transport",
        "maintenance_authority",
        "authority",
        "invocation",
    ] {
        ensure!(
            args.get(key).is_none(),
            "Account and invocation authority cannot be supplied as arguments."
        );
    }
    Ok(())
}

pub(super) fn authorize(state: &DesktopState, request: &control::Request) -> Result<Invocation> {
    let invocation = invocation(state)?;
    reject_authority_arguments(&request.args)?;
    // Ordinary native/agent calls have the same account/path limits as the browser.
    if !invocation.scope.owner
        && !(matches!(invocation.transport, Transport::Local | Transport::Window)
            && request.method == "app_quit")
    {
        if invocation.transport != Transport::Machine {
            web::check_bound_request(&invocation.scope, request)?;
        }
        check_shared_operation(request)?;
        if matches!(
            request.method.as_str(),
            "chat" | "test_providers" | "start_assistant" | "restart" | "answer_refine"
        ) {
            require_api_config(&read_config(&state.config_path)?)?;
        }
    }
    match invocation.transport {
        Transport::Local | Transport::Window => {}
        Transport::Browser => web::check_bound_request(&invocation.scope, request)?,
        Transport::Machine => ensure!(
            matches!(
                request.method.as_str(),
                "machine_register"
                    | "machine_protocol"
                    | "implementation_create"
                    | "implementation_creation"
                    | "implementation_sessions"
                    | "machine_list"
                    | "machine_teams"
                    | "machine_detail"
                    | "machine_update"
                    | "machine_revoke"
                    | "machine_directories"
                    | "start_assistant"
                    | "stop_assistant"
            ),
            "Machine credentials cannot invoke operator or browser operations."
        ),
    }
    Ok(invocation)
}

pub(super) fn check_shared_operation(request: &control::Request) -> Result<()> {
    ensure!(
        !matches!(
            request.method.as_str(),
            "azure_cli_status"
                | "github_cli_status"
                | "begin_azure_cli_login"
                | "open_azure_cli_login"
                | "finish_azure_cli_login"
                | "cancel_azure_cli_login"
                | "clone_github_repository"
                | "update_github_repository"
                | "implementation_request_catalog"
                | "implementation_request_start"
                | "implementation_open_session"
                | "implementation_create"
                | "machine_connect"
                | "machine_login"
                | "machine_login_status"
                | "machine_login_finish"
                | "machine_login_cancel"
                | "variable_launcher_define"
                | "variable_launcher_remove"
                | "variable_launcher_run"
        ),
        "{EXECUTION_DENIED}"
    );
    Ok(())
}

pub(super) fn require_api_config(config: &Config) -> Result<()> {
    ensure!(
        config
            .llm
            .active()
            .iter()
            .all(|choice| choice.provider == "deepseek"),
        "{EXECUTION_DENIED}"
    );
    ensure!(
        !config.server.cloudflare_tunnel,
        "The portal operator manages the public URL, listener and tunnel."
    );
    Ok(())
}

pub(super) fn require_account(state: &DesktopState) -> Result<()> {
    invocation(state).map(|_| ())
}

/// Also used by background/forwarded paths, before probing or starting any provider.
pub(super) fn require_execution(state: &DesktopState) -> Result<()> {
    let invocation = invocation(state)?;
    ensure!(
        invocation.scope.owner && invocation.transport != Transport::Machine,
        "{EXECUTION_DENIED}"
    );
    Ok(())
}

pub(super) fn require_operator(profile: &Path) -> Result<()> {
    if !profile.exists() {
        ensure!(
            !profile
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|n| n == "profiles")
                    && p.parent()
                        .is_some_and(|p| p.file_name().is_some_and(|n| n == "web"))),
            "Portal administration requires the local installation operator."
        );
        return Ok(());
    }
    let scope = web::account_scope(profile)?;
    ensure!(
        scope.owner && !scope.shared,
        "Portal administration requires the local installation operator."
    );
    Ok(())
}
