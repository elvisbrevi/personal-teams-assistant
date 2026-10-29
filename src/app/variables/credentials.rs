//! Compatibility for named application credentials. Variables are their only runtime store;
//! legacy stores are read once for migration and retained unchanged for recovery.
use super::*;

pub(in crate::app) fn managed(config: &Config, name: &str) -> bool {
    (name == "DEEPSEEK_API_KEY" || config.secrets.values().any(|n| n == name))
        && super::super::migration::excluded(name).is_none()
}

pub(in crate::app) fn migrate(host: &Host, store: &Store) -> std::result::Result<(), vault::Error> {
    // Signing in never moves this installation's credentials into a different account.
    if implementation::link(&host.state)?.is_some() {
        return Ok(());
    }
    let config = read_config(&host.state.config_path)?;
    super::identity::migrate(host, &config, store)?;
    for name in credential_names(&config)
        .into_iter()
        .filter(|name| managed(&config, name))
    {
        store.import_credential(
            &name,
            &format!("Application variable migrated from {name}."),
            || {
                if security::legacy_source(&name)?.is_none() {
                    return Ok(None);
                }
                Ok(Some(Secret::new(security::legacy_secret(&name)?)))
            },
        )?;
    }
    Ok(())
}

/// Compatibility CLI writes update the same entry the GUI edits, never the old store.
pub(in crate::app) async fn set(
    host: &Arc<Host>,
    name: String,
    value: String,
) -> std::result::Result<(), String> {
    let request = if let Some(link) = implementation::link(&host.state).map_err(fail)? {
        let entry = super::cached(host, &link)
            .into_iter()
            .find(|v| v.name.eq_ignore_ascii_case(&name));
        write_request(entry, name, value)
    } else {
        let store = host.state.variables.store(host).map_err(fail)?;
        migrate(host, &store).map_err(fail)?;
        let entry = store
            .list(None)
            .map_err(fail)?
            .into_iter()
            .find(|v| v.name.eq_ignore_ascii_case(&name));
        write_request(entry, name, value)
    };
    let reply = super::operate(host, &request).await;
    if reply.ok {
        Ok(())
    } else {
        Err(reply.message.unwrap_or_default())
    }
}

fn write_request(entry: Option<vault::Metadata>, name: String, value: String) -> control::Request {
    let mut request = control::Request::new(if entry.is_some() {
        "variable_update"
    } else {
        "variable_create"
    });
    request.args = match entry {
        Some(entry) => json!({"id":entry.id,"version":entry.version,"value":value}),
        None => {
            json!({"id":uuid::Uuid::new_v4().to_string(),"description":format!("Application variable for {name}."),"name":name,"value":value})
        }
    };
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AccountSecrets;

    #[tokio::test]
    async fn application_lookups_and_compatibility_writes_use_the_same_variables() {
        let root = tempfile::tempdir().unwrap();
        let host = super::super::tests::host(root.path());
        let mut config = read_config(&host.state.config_path).unwrap();
        config
            .secrets
            .insert("secret://azure/reader".into(), "AZURE_DEVOPS_TOKEN".into());
        write_private(
            &host.state.config_path,
            &toml::to_string_pretty(&config).unwrap(),
        )
        .unwrap();
        assert!(managed(&config, "AZURE_DEVOPS_TOKEN"));
        for name in [
            "STATE_ENCRYPTION_KEY",
            "GRAPH_WEBHOOK_SECRET",
            "PTA_MACHINE_TOKEN",
            "CLOUDFLARE_TUNNEL_TOKEN",
            "GITHUB_OAUTH_TOKENS",
        ] {
            config
                .secrets
                .insert(format!("secret://test/{name}"), name.into());
            assert!(!managed(&config, name));
        }
        let store = host.state.variables.store(&host).unwrap();
        store
            .import_credential("AZURE_DEVOPS_TOKEN", "Azure access", || {
                Ok(Some(Secret::new("synthetic-original-value")))
            })
            .unwrap();
        let resolver = consumers::Resolver::new(&host);
        assert!(resolver.bound("AZURE_DEVOPS_TOKEN").unwrap());
        assert!(resolver.present("AZURE_DEVOPS_TOKEN").unwrap());
        assert_eq!(
            resolver.value("AZURE_DEVOPS_TOKEN").unwrap(),
            "synthetic-original-value"
        );
        set(
            &host,
            "AZURE_DEVOPS_TOKEN".into(),
            "synthetic-replacement-value".into(),
        )
        .await
        .unwrap();
        let entry = store.list(None).unwrap().remove(0);
        assert_eq!(entry.version, 2);
        assert_eq!(
            resolver.value("AZURE_DEVOPS_TOKEN").unwrap(),
            "synthetic-replacement-value"
        );
        delete_credential(&host, "AZURE_DEVOPS_TOKEN".into())
            .await
            .unwrap();
        assert!(!resolver.present("AZURE_DEVOPS_TOKEN").unwrap());
        assert!(resolver.value("AZURE_DEVOPS_TOKEN").is_err());
        set(
            &host,
            "AZURE_DEVOPS_TOKEN".into(),
            "synthetic-new-value".into(),
        )
        .await
        .unwrap();
        assert_eq!(
            resolver.value("AZURE_DEVOPS_TOKEN").unwrap(),
            "synthetic-new-value"
        );
    }
}
