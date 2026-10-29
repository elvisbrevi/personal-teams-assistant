//! Public Microsoft setup identifiers backed by encrypted account variables. Sessions stay local.
use super::*;

pub(in crate::app) const FIELDS: [(&str, &str); 3] = [
    (
        "ENTRA_TENANT_ID",
        "Entra tenant ID used by the assistant's Microsoft registration.",
    ),
    (
        "ENTRA_CLIENT_ID",
        "Entra client ID of the assistant's public desktop application.",
    ),
    (
        "TEAMS_USER_ID",
        "Microsoft Teams user ID verified when connecting the Microsoft account.",
    ),
];

pub(in crate::app) fn value<'a>(config: &'a Config, name: &str) -> Option<&'a str> {
    let index = FIELDS.iter().position(|(field, _)| *field == name)?;
    let value = values(config)[index];
    let id = uuid::Uuid::parse_str(value).ok()?;
    // The example registration and desktop template are not a person's configuration.
    (id.as_u128() > 3).then_some(value)
}

fn values(config: &Config) -> [&str; 3] {
    [
        &config.graph.tenant_id,
        &config.graph.client_id,
        &config.graph.user_id,
    ]
}

pub(in crate::app) fn migrate(
    host: &Host,
    config: &Config,
    store: &Store,
) -> std::result::Result<(), vault::Error> {
    // Restored public identifiers are readable local setup, never a new legacy import into
    // this machine's own store after sign-out or a switch of assistant account.
    if restored_path(host).exists() {
        return Ok(());
    }
    for (name, description) in FIELDS {
        if let Some(value) = value(config, name) {
            store.import_credential(name, description, || Ok(Some(Secret::new(value))))?;
        }
    }
    Ok(())
}

fn restored_path(host: &Host) -> PathBuf {
    host.state
        .config_path
        .parent()
        .unwrap()
        .join("variables/microsoft-setup-account.json")
}

fn signature(
    config: &Config,
    link: &Option<implementation::Link>,
    entries: &[vault::Metadata],
) -> String {
    // Only public setup identifiers, account-qualified references and versions; never credentials.
    json!([
        link,
        values(config),
        entries
            .iter()
            .map(|v| (&v.id, v.version))
            .collect::<Vec<_>>()
    ])
    .to_string()
}

/// Called with the profile operations lock held. A bad/conflicting identity never makes the
/// whole GUI unreadable and never replaces an existing Teams database's identity.
pub(in crate::app) async fn restore(host: &Host) -> std::result::Result<(), String> {
    authority::require_account(&host.state).map_err(fail)?;
    let state = &host.state;
    let link = implementation::link(state).map_err(fail)?;
    let config = read_config(&state.config_path).map_err(fail)?;
    let entries = if let Some(link) = &link {
        super::cached(host, link)
    } else {
        let store = state.variables.store(host).map_err(fail)?;
        migrate(host, &config, &store).map_err(fail)?;
        store.list(None).map_err(fail)?
    };
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|v| {
            FIELDS
                .iter()
                .any(|(name, _)| v.name.eq_ignore_ascii_case(name))
        })
        .collect();
    let key = signature(&config, &link, &entries);
    if state.variables.identity.lock().unwrap().as_ref() == Some(&key) {
        return Ok(());
    }
    let mut next = config.clone();
    for entry in &entries {
        let (_, value) = consumers::resolve_entry(host, &entry.id, Some(entry.version))
            .await
            .map_err(fail)?;
        let value = value.expose();
        if !uuid::Uuid::parse_str(value).is_ok_and(|id| id.as_u128() > 3) {
            return Err(
                "A Microsoft setup variable is invalid. Enter a real UUID in Variables.".into(),
            );
        }
        match entry.name.to_ascii_uppercase().as_str() {
            "ENTRA_TENANT_ID" => next.graph.tenant_id = value.into(),
            "ENTRA_CLIENT_ID" => next.graph.client_id = value.into(),
            "TEAMS_USER_ID" => next.graph.user_id = value.into(),
            _ => unreachable!(),
        }
    }
    if implementation::link(state).map_err(fail)? != link {
        return Err("The signed-in account changed. Synchronize again.".into());
    }
    authority::require_account(state).map_err(fail)?;
    if link.is_some() && !entries.is_empty() {
        write_private(
            &restored_path(host),
            &serde_json::to_string(&link).map_err(fail)?,
        )
        .map_err(fail)?;
    }
    if values(&next) != values(&config) {
        web::validate_profile_identity(state.config_path.parent().unwrap(), &next).map_err(
            |_| {
                "The account's Microsoft setup conflicts with this computer's configuration."
                    .to_owned()
            },
        )?;
        protect_identity(&config, &next).map_err(|_| "The account's Microsoft setup differs from this computer's Teams database. Use a fresh profile to connect that identity.".to_owned())?;
        next.validate().map_err(|_| {
            "The account's Microsoft setup conflicts with this computer's configuration.".to_owned()
        })?;
        if state.running.lock().await.is_some() {
            return Err(
                "Stop the assistant before restoring Microsoft setup from Variables.".into(),
            );
        }
        write_private(
            &state.config_path,
            &toml::to_string_pretty(&next).map_err(fail)?,
        )
        .map_err(fail)?;
        *state.activity_graph.lock().await = None;
    }
    *state.variables.identity.lock().unwrap() = Some(signature(&next, &link, &entries));
    Ok(())
}

/// A normal edit of the setup fields writes the same variables, under the versions just read.
/// Signing in alone never uploads another computer's existing setup to the account.
pub(in crate::app) async fn save(
    host: &Arc<Host>,
    before: &Config,
    after: &Config,
) -> std::result::Result<(), String> {
    if values(before) == values(after) {
        return Ok(());
    }
    let entries = if implementation::link(&host.state).map_err(fail)?.is_some() {
        let reply = super::operate(host, &control::Request::new("variable_sync")).await;
        if !reply.ok {
            return Err(reply.message.unwrap_or(reply.code));
        }
        let link = implementation::link(&host.state)
            .map_err(fail)?
            .ok_or("The signed-in account changed. Synchronize again.")?;
        super::cached(host, &link)
    } else {
        let store = host.state.variables.store(host).map_err(fail)?;
        migrate(host, before, &store).map_err(fail)?;
        store.list(None).map_err(fail)?
    };
    for (index, (name, description)) in FIELDS.iter().enumerate() {
        if values(before)[index] == values(after)[index] {
            continue;
        }
        let Some(value) = value(after, name) else {
            continue;
        };
        let existing = entries.iter().find(|v| v.name.eq_ignore_ascii_case(name));
        if let Some(existing) = existing {
            let (_, stored) = consumers::resolve_entry(host, &existing.id, Some(existing.version))
                .await
                .map_err(fail)?;
            if stored.expose() == value {
                if implementation::link(&host.state).map_err(fail)?.is_none() {
                    host.state
                        .variables
                        .store(host)
                        .map_err(fail)?
                        .mark_credential_managed(name)
                        .map_err(fail)?;
                }
                continue;
            }
            if Some(stored.expose()) != self::value(before, name) {
                return Err(
                    "Microsoft setup changed in Variables. Refresh the app before saving it again."
                        .into(),
                );
            }
        }
        let mut request = control::Request::new(if existing.is_some() {
            "variable_update"
        } else {
            "variable_create"
        });
        request.args = match existing {
            Some(entry) => json!({"id":entry.id,"version":entry.version,"value":value}),
            None => {
                json!({"id":uuid::Uuid::new_v4().to_string(),"name":name,"description":description,"value":value})
            }
        };
        let reply = Box::pin(super::operate(host, &request)).await;
        if !reply.ok {
            return Err(reply.message.unwrap_or(reply.code));
        }
        if implementation::link(&host.state).map_err(fail)?.is_none() {
            host.state
                .variables
                .store(host)
                .map_err(fail)?
                .mark_credential_managed(name)
                .map_err(fail)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
