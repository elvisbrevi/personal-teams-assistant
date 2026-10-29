use super::*;
use crate::app::{
    authority::Transport::Local,
    variables::tests::{call, host},
};

const TENANT: &str = "11111111-1111-4111-8111-111111111111";
const CLIENT: &str = "22222222-2222-4222-8222-222222222222";
const USER: &str = "33333333-3333-4333-8333-333333333333";

fn configured(host: &Host) -> Config {
    let mut config = read_config(&host.state.config_path).unwrap();
    config.graph.tenant_id = TENANT.into();
    config.graph.client_id = CLIENT.into();
    config.graph.user_id = USER.into();
    write_private(&host.state.config_path, &toml::to_string(&config).unwrap()).unwrap();
    config
}

#[tokio::test]
async fn existing_setup_is_imported_once_encrypted_and_configuration_edits_update_variables() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let before = configured(&host);
    let list = call(&host, Local, "variable_list", json!({})).await;
    assert!(list.ok, "{:?}", list.message);
    assert_eq!(list.data["total"], 3);
    assert!(!serde_json::to_string(&list).unwrap().contains(TENANT));
    let store = host.state.variables.store(&host).unwrap();
    assert_eq!(store.resolve("ENTRA_CLIENT_ID").unwrap().expose(), CLIENT);
    let mut next = before.clone();
    next.graph.client_id = "44444444-4444-4444-8444-444444444444".into();
    save(&host, &before, &next).await.unwrap();
    assert_eq!(
        store.resolve("ENTRA_CLIENT_ID").unwrap().expose(),
        next.graph.client_id
    );
    restore(&host).await.unwrap();
    assert_eq!(
        read_config(&host.state.config_path)
            .unwrap()
            .graph
            .client_id,
        next.graph.client_id
    );
    let entry = store
        .list(None)
        .unwrap()
        .into_iter()
        .find(|v| v.name == "ENTRA_CLIENT_ID")
        .unwrap();
    store.delete(&entry.id, entry.version).unwrap();
    migrate(&host, &before, &store).unwrap();
    assert!(store.resolve("ENTRA_CLIENT_ID").is_err());
}

#[tokio::test]
async fn a_new_signed_in_machine_restores_setup_without_copying_the_microsoft_session() {
    let root = tempfile::tempdir().unwrap();
    let pair = super::super::sync_tests::pair(root.path(), None).await;
    configured(&pair.coordinator);
    // The machine catalog imports the coordinator's existing setup even before Variables is opened.
    assert!(
        call(&pair.native, Local, "variable_sync", json!({}))
            .await
            .ok
    );
    restore(&pair.native).await.unwrap();
    let config = read_config(&pair.native.state.config_path).unwrap();
    assert_eq!(values(&config), [TENANT, CLIENT, USER]);
    assert!(!crate::state::Store::has_token(&config.server.data_dir.join("assistant.db")).unwrap());
    // A second read requires no more variable uses when the same versions are already applied.
    restore(&pair.native).await.unwrap();
    let mirror = fs::read_to_string(pair.profile.join("variables/catalog-mirror.json")).unwrap();
    assert!(!mirror.contains(TENANT) && !mirror.contains(CLIENT) && !mirror.contains(USER));
    // Signing out never reimports the former account's restored IDs into this machine's store.
    fs::remove_file(pair.profile.join("implementation/connection.json")).unwrap();
    pair.native.state.variables.forget_catalog();
    let local_store = pair.native.state.variables.store(&pair.native).unwrap();
    migrate(&pair.native, &config, &local_store).unwrap();
    assert!(local_store.list(None).unwrap().is_empty());
}

#[tokio::test]
async fn invalid_and_conflicting_setup_preserve_the_existing_database_identity() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let config = configured(&host);
    let store = host.state.variables.store(&host).unwrap();
    migrate(&host, &config, &store).unwrap();
    let entry = store
        .list(None)
        .unwrap()
        .into_iter()
        .find(|v| v.name == "ENTRA_TENANT_ID")
        .unwrap();
    store
        .update(
            &entry.id,
            entry.version,
            None,
            Some(&Secret::new("not-a-uuid")),
        )
        .unwrap();
    assert!(restore(&host).await.unwrap_err().contains("invalid"));
    assert_eq!(
        read_config(&host.state.config_path)
            .unwrap()
            .graph
            .tenant_id,
        TENANT
    );
    store
        .update(
            &entry.id,
            entry.version + 1,
            None,
            Some(&Secret::new(CLIENT)),
        )
        .unwrap();
    fs::write(config.server.data_dir.join("assistant.db"), []).unwrap();
    assert!(restore(&host).await.unwrap_err().contains("Teams database"));
    assert_eq!(
        read_config(&host.state.config_path)
            .unwrap()
            .graph
            .tenant_id,
        TENANT
    );
}

#[test]
fn template_values_are_never_migrated() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let config = read_config(&host.state.config_path).unwrap();
    let store = host.state.variables.store(&host).unwrap();
    migrate(&host, &config, &store).unwrap();
    assert!(store.list(None).unwrap().is_empty());
}

#[tokio::test]
async fn deleting_setup_saved_on_a_fresh_installation_never_reimports_it() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let before = read_config(&host.state.config_path).unwrap();
    let mut next = before.clone();
    next.graph.tenant_id = TENANT.into();
    save(&host, &before, &next).await.unwrap();
    let store = host.state.variables.store(&host).unwrap();
    let entry = store.list(None).unwrap().remove(0);
    store.delete(&entry.id, entry.version).unwrap();
    migrate(&host, &next, &store).unwrap();
    assert!(store.list(None).unwrap().is_empty());
}

#[tokio::test]
async fn an_existing_different_variable_is_reported_without_overwriting_either_copy() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    configured(&host);
    let created = call(
        &host,
        Local,
        "variable_create",
        crate::app::variables::tests::create("ENTRA_TENANT_ID", CLIENT),
    )
    .await;
    assert!(created.ok);
    assert!(restore(&host).await.is_err());
    let store = host.state.variables.store(&host).unwrap();
    assert_eq!(store.resolve("ENTRA_TENANT_ID").unwrap().expose(), CLIENT);
    assert_eq!(
        read_config(&host.state.config_path)
            .unwrap()
            .graph
            .tenant_id,
        TENANT
    );
    let list = call(&host, Local, "variable_list", json!({})).await;
    assert!(list.ok);
    assert!(
        list.data["migration_error"]
            .as_str()
            .unwrap()
            .contains("differs")
    );
}
