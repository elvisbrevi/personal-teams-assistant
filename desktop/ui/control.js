// The control contract: every operation of the GUI is one request to the host (`invoke`), on
// the owner's local page and in the portal alike. Typed (docs/ui-design-conventions.md §11).
import { transport } from './transport.js';
import { t } from './i18n.js';
import { current } from './state.js';
import { notify } from './notices.js';
import { showTab } from './nav.js';
import { browserMicrosoftLogin } from './dialogs.js';
/** @import { Transport } from './transport.js' */

/**
 * The operations the GUI asks the host for (the `method` of a request). A new one is added here,
 * so a misspelled method fails the type check instead of the host.
 * @typedef {(
 *   'account_sync' | 'activity_history' | 'activity_open' | 'activity_pending' | 'activity_run' |
 *   'activity_status' | 'answer_feedback' | 'answer_refine' | 'answer_send' | 'audit' |
 *   'azure_cli_status' | 'begin_azure_cli_login' | 'begin_github_login' |
 *   'cancel_azure_cli_login' | 'cancel_microsoft' | 'chat' | 'clone_github_repository' |
 *   'cloud_backups' | 'cloud_conflicts' | 'cloud_connect' | 'cloud_disconnect' | 'cloud_erase' |
 *   'cloud_installations' | 'cloud_lost' | 'cloud_migrate' | 'cloud_promote' | 'cloud_repair' |
 *   'cloud_resolve' | 'cloud_restore' | 'cloud_revoke' | 'cloud_rollback' | 'cloud_rotate_key' |
 *   'cloud_status' | 'cloud_token' | 'cloud_verify' | 'connect_microsoft' | 'crate_update_apply' |
 *   'crate_update_cancel' | 'crate_update_check' | 'crate_update_dismiss' | 'crate_update_stage' |
 *   'crate_update_status' | 'disconnect_github' | 'finish_azure_cli_login' |
 *   'finish_github_login' | 'finish_microsoft' | 'get_azure_hu' | 'get_azure_task' |
 *   'get_github_epic' | 'get_github_issue' | 'github_cli_status' | 'github_repositories' |
 *   'implementation_browse' | 'implementation_cancel' | 'implementation_catalog' |
 *   'implementation_create' | 'implementation_creation' | 'implementation_history' |
 *   'implementation_mobile_session' | 'implementation_open_session' | 'implementation_prepare' |
 *   'implementation_request_catalog' | 'implementation_request_start' |
 *   'implementation_request_status' | 'implementation_runs' | 'implementation_sessions' |
 *   'implementation_start' | 'import_existing' | 'index_refresh' | 'index_status' |
 *   'list_azure_hus' | 'list_azure_tasks' | 'list_github_epics' | 'list_github_issues' |
 *   'llm_providers' | 'machine_assistant' | 'machine_browse' | 'machine_detail' |
 *   'machine_directories' | 'machine_disconnect' | 'machine_list' | 'machine_login' |
 *   'machine_login_cancel' | 'machine_login_finish' | 'machine_revoke' | 'machine_save' |
 *   'machine_settings' | 'machine_teams' | 'machine_update' | 'notes_list' | 'notes_remove' |
 *   'notes_save' | 'open_azure_cli_login' | 'open_github_login' | 'open_work_item' |
 *   'portable_status' | 'question_associate' | 'restart' | 'save_settings' | 'self_chat_disable' |
 *   'self_chat_enable' | 'self_chat_status' | 'setup_readiness' | 'snapshot' | 'start_assistant' |
 *   'stop_assistant' | 'test_self_chat' | 'update_github_repository' | 'variable_consumer_bind' |
 *   'variable_consumer_check' | 'variable_consumer_list' | 'variable_consumer_unbind' |
 *   'variable_create' | 'variable_delete' | 'variable_deliver' | 'variable_delivery_approve' |
 *   'variable_delivery_list' | 'variable_delivery_remove' | 'variable_entry_cancel' |
 *   'variable_entry_complete' | 'variable_launcher_approve' | 'variable_launcher_remove' |
 *   'variable_launcher_run' | 'variable_list' | 'variable_migration_inspect' |
 *   'variable_migration_run' | 'variable_rename' | 'variable_reveal' | 'variable_update'
 * )} Method
 */
/**
 * Every `code` a reply carries (src/app/control.rs and the operations behind it): `ok`, or why
 * it failed. `authorization_pending` is not a failure: a sign-in waits for the browser.
 * @typedef {'ok' | 'authorization_pending' | 'invalid_input' | 'not_ready' | 'state_conflict'
 *   | 'revision_conflict' | 'conflict' | 'authority_denied' | 'operation_failed'
 *   | 'dependency_or_network' | 'dependency_or_state' | 'contract_mismatch'} ReplyCode
 */
/**
 * One request of the contract (`control::Request`). `revision` is the profile revision the form
 * was loaded at, or `null` for operations that neither check nor advance it.
 * @typedef {object} Request
 * @property {1} contract
 * @property {Method} method
 * @property {object} args The operation's arguments, a JSON object.
 * @property {string | null} [revision]
 */
/**
 * The host's reply (`control::Reply`). `data` is the operation's own JSON, read by the module that
 * asked for it; `revision` is the profile revision after the operation.
 * @typedef {object} Reply
 * @property {number} contract
 * @property {string} version
 * @property {boolean} ok
 * @property {ReplyCode} code
 * @property {number} exit_code
 * @property {string | null} message
 * @property {any} data
 * @property {string | null} revision
 */

/** @type {Transport['invoke']} */
const rawInvoke = transport.invoke;
// Also the answer operations: they do not depend on the profile files, so they neither check
// nor advance the form's revision.
/** @type {Method[]} */
const READS = ['snapshot', 'chat', 'github_repositories', 'self_chat_status', 'llm_providers', 'azure_cli_status', 'begin_azure_cli_login', 'open_azure_cli_login', 'finish_azure_cli_login', 'cancel_azure_cli_login', 'github_cli_status', 'audit', 'activity_status', 'activity_history', 'activity_pending', 'answer_refine', 'answer_send', 'list_azure_hus', 'list_github_epics', 'list_github_issues', 'get_azure_hu', 'get_azure_task', 'list_azure_tasks', 'get_github_epic', 'get_github_issue', 'open_work_item'];
// Revision of the profile files the last reply saw. The form's own revision (`current.revision`)
// changes only when it reloads or writes, so saving a stale form fails instead of overwriting
// changes made elsewhere (e.g. with `pta`).
/** @type {string | null} */
let lastRevision = null;
/**
 * Sends one request and returns its `data`; a failed reply throws its message (or code). Writes
 * carry the form's revision so a stale form fails instead of overwriting newer changes, and a
 * sign-in that waits for the browser is followed until it finishes.
 * @param {Method} method
 * @param {object} [args]
 * @returns {Promise<any>}
 */
async function invoke(method, args = {}) {
  if (method === 'activity_open') {
    showTab('registration');
    return { visible: true };
  }
  /** @type {Reply} */
  let result = await rawInvoke('command', { request: {
    method, args, revision: READS.includes(method) || /^(implementation_|machine_|variable_|crate_update_|cloud_)/.test(method) ? null : current?.revision ?? null, contract: 1
  }});
  if (transport.browser && result.code === 'authorization_pending' && method === 'connect_microsoft') {
    result = await browserMicrosoftLogin(result.data.authorization_url);
  }
  if (result.code === 'authorization_pending' && ['connect_microsoft', 'finish_github_login'].includes(method)) {
    /** @type {Method} */
    const finish = method === 'connect_microsoft' ? 'finish_microsoft' : 'finish_github_login';
    const deadline = Date.now() + 600_000;
    notify(t('Authorization pending in the browser.'), 'info');
    while (result.code === 'authorization_pending' && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 2000));
      result = await rawInvoke('command', { request: { method: finish, args: {}, revision: null, contract: 1 } });
    }
  }
  if (result.revision) lastRevision = result.revision;
  if (result.revision && current && !READS.includes(method) && !/^(implementation_|machine_|variable_|crate_update_|cloud_)/.test(method)) current.revision = result.revision;
  if (!result.ok) throw result.message || result.code;
  return result.data;
}

export { rawInvoke, lastRevision, invoke };
