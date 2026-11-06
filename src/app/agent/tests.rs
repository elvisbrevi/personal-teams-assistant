use super::*;
use crate::app::tools;

#[test]
fn every_catalog_tool_is_classified_once_and_a_new_tool_is_denied_until_classified() {
    for tool in tools::TOOLS {
        let found = TOOLS.iter().filter(|(name, _)| *name == tool.name).count();
        assert_eq!(
            found, 1,
            "{} must be classified exactly once for the remote endpoint",
            tool.name
        );
    }
    for (name, _) in TOOLS {
        assert!(
            tools::TOOLS.iter().any(|tool| tool.name == *name),
            "{name} is classified but not in the catalog"
        );
    }
    assert_eq!(
        tool("a_tool_added_to_the_catalog_later"),
        Remote::Denied(UNCLASSIFIED)
    );
}

#[test]
fn a_remote_agent_is_refused_what_only_a_person_or_the_owners_computer_may_do() {
    for name in [
        // Launchers on the shared server.
        "launcher_define",
        "launcher_run",
        "launcher_remove",
        // Updates.
        "update_check",
        "update_stage",
        "update_apply",
        "update_cancel",
        "update_dismiss",
        // Key custody and the account store's lifecycle.
        "cloud_rotate_key",
        "cloud_lost",
        "cloud_connect",
        "cloud_disconnect",
        "cloud_erase",
        "cloud_restore",
        "cloud_revoke",
        "cloud_promote",
        "cloud_migrate",
        "cloud_rollback",
        "migration_run",
        // The host computer itself.
        "machine_connect",
        "machine_disconnect",
        "machine_login",
        "machine_login_finish",
        "machine_save",
        "machine_browse",
        "begin_azure_cli_login",
        "open_work_item",
        "implementation_open_session",
    ] {
        assert!(
            matches!(tool(name), Remote::Denied(_)),
            "{name} must be denied"
        );
    }
    // Operations no tool reaches, refused by the host on this transport whatever sends them.
    for method in [
        "variable_reveal",
        "variable_create",
        "variable_entry_complete",
        "variable_launcher_approve",
        "variable_launcher_define",
        "variable_launcher_run",
        "variable_delivery_approve",
        "variable_key_backup",
        "variable_key_restore",
        "variable_key_rotate",
        "variable_audit",
        "variable_verify",
        "crate_update_check",
        "crate_update_stage",
        "crate_update_apply",
        "cloud_rotate_key",
        "set_credential",
        "delete_credential",
        "machine_register",
        "machine_protocol",
        "machine_connect",
        "teams_post",
        "app_quit",
        "logs",
        "import_existing",
        "test_providers",
    ] {
        assert!(check_method(method).is_err(), "{method} must be refused");
    }
    for method in METHODS {
        assert!(
            !NEVER.contains(method),
            "{method} is both allowed and never allowed"
        );
        assert!(check_method(method).is_ok());
    }
}

#[test]
fn the_endpoint_offers_reading_and_the_accounts_own_work() {
    for name in [
        "status",
        "notes_search",
        "note_save",
        "variable_list",
        "variable_entry_request",
        "variable_rotation_request",
        "machine_list",
        "implementation_start",
        "implementation_runs",
        "answer_send",
    ] {
        assert_eq!(tool(name), Remote::Offered, "{name}");
    }
}
