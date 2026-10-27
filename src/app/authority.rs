//! Authority comes from the owning profile and authenticated transport, never arguments.
use super::*;
use serde_json::Value;

pub(super) const EXECUTION_DENIED: &str = "Shared-host execution requires a verified OS security boundary. Use your personal trusted machine; a working directory or tool-less prompt is not an OS sandbox.";

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transport {
    Local,
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
        transport: Transport::Local,
    });
    ensure!(invocation.scope == scope, "Account binding mismatch.");
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
    ensure!(invocation.scope.owner, "{EXECUTION_DENIED}");
    match invocation.transport {
        Transport::Local => {}
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
                    | "start_assistant"
                    | "stop_assistant"
            ),
            "Machine credentials cannot invoke operator or browser operations."
        ),
    }
    Ok(invocation)
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
