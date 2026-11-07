//! Account-authoritative machine metadata. Filesystem changes execute on the target.
use super::paths;
use super::*;
use crate::store::settings::SettingsStore;

pub(super) fn version(host: &Host) -> Result<String> {
    Ok(authority::invocation(&host.state)?
        .scope
        .account
        .map(|a| a.version)
        .unwrap_or_default())
}

fn summary(machine: &Machine) -> Value {
    let online = !machine.revoked && now() - machine.seen < 35;
    json!({"id":machine.id,"name":machine.name,"os":machine.os,"local":false,
        "trust":if machine.revoked {"revoked"} else {"trusted"},"online":online,
        "presence":if !online {"offline"} else if machine.assistant_phase.is_none() {"unknown"} else {"online"},
        "last_seen":machine.seen,"last_known":!online,"assistant_phase":machine.assistant_phase,
        "capabilities":{"management":machine.management_support,"directories":machine.management_support,"implementation":machine.management_support,"assistant":machine.assistant_phase.is_some()},
        "roots":if machine.revoked {Vec::new()} else {machine.roots.clone()},
        "version":machine.host_version,"update":machine.update})
}

pub(super) async fn list(host: &Arc<Host>) -> Result<Value> {
    let version = version(host)?;
    let state = &host.state.implementation;
    let data = state.registry.lock().await;
    let config = host.state.settings.config()?;
    let local = json!({"id":data.id,"name":data.settings.name,"os":std::env::consts::OS,
        "online":true,"local":true,"presence":"online","trust":"trusted","last_seen":now(),"last_known":false,
        "assistant_phase":*host.state.phase.borrow(),"roots":data.settings.roots,
        "version":env!("CARGO_PKG_VERSION"),"update":crate::app::maintenance::report(host),
        "capabilities":{"management":true,"directories":config.server.webhook_prefix.is_empty(),"implementation":authority::invocation(&host.state)?.scope.owner,"assistant":true}});
    let mut machines = vec![local];
    machines.extend(
        data.machines
            .iter()
            .filter(|m| m.account_version == version)
            .map(summary),
    );
    Ok(
        json!({"machines":machines,"teams_machine":data.teams_machine,"this_machine":data.id,"authoritative":true}),
    )
}

pub(super) fn revoke(data: &mut Registry, id: &str) -> Result<()> {
    let machine = data
        .machines
        .iter_mut()
        .find(|m| m.id == id)
        .context("Machine is not registered.")?;
    machine.revoked = true;
    machine.secret_hash.clear();
    machine.roots.clear();
    if data.teams_machine == id {
        data.teams_machine = data.id.clone();
    }
    if let Some((held, _)) = &mut data.teams_lease
        && held == id
    {
        *held = format!("revoked:{id}");
    }
    // Keep the old expiry: an offline sender must stop before any successor starts.
    Ok(())
}

pub(super) async fn save_roots(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let roots: Vec<PathBuf> = serde_json::from_value(args["roots"].clone())?;
    ensure!(roots.len() <= 16, "Select at most sixteen allowed roots.");
    let state = &host.state.implementation;
    let config = host.state.settings.config()?;
    let mut data = state.registry.lock().await;
    ensure!(
        config.server.webhook_prefix.is_empty()
            || paths::plain_all(&roots) == paths::plain_all(&data.settings.roots),
        "The portal operator manages this machine's allowed directories."
    );
    let mut canonical = Vec::new();
    for path in roots {
        ensure!(
            path.is_absolute() && path.as_os_str().len() <= 4096,
            "Allowed roots must be existing absolute directories."
        );
        // Reject aliases that could escape the directory selected in the sheet.
        for component in path.ancestors() {
            ensure!(
                !fs::symlink_metadata(component)?.file_type().is_symlink(),
                "Allowed roots cannot contain symbolic links."
            );
        }
        let canonical_path = path.canonicalize()?;
        ensure!(
            canonical_path.is_dir(),
            "Allowed roots must be existing absolute directories."
        );
        // Stored and shown in the plain spelling; checks canonicalize it again.
        let path = paths::plain(&canonical_path);
        if !canonical.contains(&path) {
            canonical.push(path);
        }
    }
    data.settings.roots = canonical;
    state.save(&data)?;
    Ok(json!({"roots":data.settings.roots,"saved":true}))
}

/// Revoke a machine on this coordinator and drop its pending calls; returns its installation in the
/// account store (empty: none). Repeating it on a revoked machine changes nothing more.
pub(super) async fn revoke_now(host: &Arc<Host>, id: &str) -> Result<String> {
    let state = &host.state.implementation;
    let mut data = state.registry.lock().await;
    let store = data
        .machines
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.store_installation.clone())
        .unwrap_or_default();
    revoke(&mut data, id)?;
    state.save(&data)?;
    drop(data);
    state.calls.lock().await.retain(|c| c.machine != id);
    Ok(store)
}

pub(super) async fn operate(host: &Arc<Host>, method: &str, args: &Value) -> Result<Value> {
    if let Some(result) = fleet::coordinator(host, method, args).await? {
        return Ok(result);
    }
    let id = args["machine"].as_str().context("Choose a machine.")?;
    uuid(id)?;
    let version = version(host)?;
    let state = &host.state.implementation;
    let own = state.registry.lock().await.id.clone();
    let detail = list(host).await?["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .cloned()
        .context("Machine is not registered.")?;
    if method == "machine_detail" {
        return Ok(detail);
    }
    ensure!(
        detail["trust"] == "trusted",
        "Machine has been revoked. Sign in and trust it again before use."
    );
    match method {
        "machine_revoke" => {
            ensure!(
                id != own,
                "The coordinator cannot revoke itself. Ask the portal operator to revoke the account instead."
            );
            let store = revoke_now(host, id).await?;
            let mut reply = json!({"revoked":true,"machine":id});
            if !store.is_empty() {
                reply["store"] = super::super::cloud::revoke_installation(host, &store).await;
            }
            Ok(reply)
        }
        "machine_directories" => {
            ensure!(
                detail["online"] == true && detail["capabilities"]["directories"] == true,
                "Directory management requires an online compatible machine."
            );
            fleet::call(host, id, "machine_browse", json!({"path":args["path"]})).await
        }
        "machine_update" => {
            let name = args
                .get("name")
                .map(|v| v.as_str().context("Choose a machine name."))
                .transpose()?;
            if let Some(name) = name {
                ensure!(
                    !name.trim().is_empty()
                        && name.len() <= 100
                        && !name.chars().any(char::is_control),
                    "Choose a machine name."
                );
            }
            ensure!(
                name.is_some() || args.get("roots").is_some(),
                "Choose a name or allowed roots to change."
            );
            let roots = if args.get("roots").is_some() {
                ensure!(
                    detail["online"] == true
                        && (id == own || detail["capabilities"]["directories"] == true),
                    "Directory management requires an online compatible machine."
                );
                Some(fleet::call(host, id, "machine_roots", json!({"roots":args["roots"]})).await?["roots"].clone())
            } else {
                None
            };
            let mut data = state.registry.lock().await;
            if id == own {
                if let Some(name) = name {
                    data.settings.name = name.into();
                }
            } else {
                let machine = data
                    .machines
                    .iter_mut()
                    .find(|m| m.id == id && !m.revoked && m.account_version == version)
                    .context("Machine is not registered.")?;
                if let Some(name) = name {
                    machine.name = name.into();
                }
                if let Some(roots) = roots {
                    machine.roots =
                        paths::plain_all(&serde_json::from_value::<Vec<PathBuf>>(roots)?);
                }
            }
            state.save(&data)?;
            Ok(json!({"saved":true,"machine":id}))
        }
        _ => anyhow::bail!("Unknown machine management operation."),
    }
}
