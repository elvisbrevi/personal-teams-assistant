// The shared state of the GUI: the snapshot the forms edit, the saved one, readiness, the open
// section and unsaved edits. Other modules read these bindings and change them only through the
// setters below. Typed (docs/ui-design-conventions.md §11).

/**
 * The profile as the host's `snapshot` returns it (`Snapshot` in src/app.rs). `config` and `map`
 * keep the shapes of `Config` and `KnowledgeMap` and are read field by field by the modules that
 * edit them. `revision` is the one the form was loaded at (set from the reply, `lastRevision`).
 * @typedef {object} Snapshot
 * @property {{ id: string, username: string, shared: boolean, local_execution: boolean } | null} account
 * @property {string | null} revision
 * @property {Record<string, any> | null} loaded_config
 * @property {string} host_version
 * @property {boolean} host_running
 * @property {boolean} headless
 * @property {string} profile
 * @property {boolean} tunnel_running
 * @property {boolean} microsoft_connected
 * @property {string | null} teams_identity_error
 * @property {Record<string, any>} config
 * @property {Record<string, any>} map
 * @property {Record<string, string>} credentials
 * @property {boolean} running
 * @property {number} active_subscriptions
 * @property {string | null} subscription_issue
 * @property {boolean} github_connected
 * @property {string} tunnel_config
 * @property {any[]} cases
 * @property {any[]} agent_prompts
 */
/**
 * One step of this installation's readiness (src/app/readiness.rs).
 * @typedef {object} ReadinessStep
 * @property {string} id
 * @property {'coordinator' | 'this_machine'} scope
 * @property {boolean} required
 * @property {string} state `done`, `todo`, `waiting`, `attention`, `unknown`…
 * @property {string} detail
 * @property {Record<string, any>} facts
 * @property {Record<string, any>[]} actions
 */
/**
 * How far this installation is from being a working member of the account (`setup_readiness`).
 * @typedef {object} Readiness
 * @property {boolean} ready
 * @property {'portal_account' | 'signed_in' | 'standalone'} mode
 * @property {'browser' | 'window' | 'agent' | 'local'} client
 * @property {boolean} local_owner
 * @property {string | null} coordinator
 * @property {boolean} coordinating
 * @property {ReadinessStep[]} steps
 * @property {{ step: string, state: string, detail: string, actions: Record<string, any>[] } | null} next
 * @property {Record<string, any>} agent
 */
/**
 * A section of the GUI: a navigation destination or a view inside one (To-Do's views).
 * @typedef {'overview' | 'todo' | 'create-request' | 'agent-history' | 'agent-guides' | 'implementation'
 *   | 'messages' | 'registration' | 'workflows' | 'knowledge' | 'chat' | 'settings'} Section
 */

/** Snapshot being edited by the forms (saved with «Save changes»). @type {Snapshot | undefined} */
let current;
/** Latest persisted snapshot: drives status, overview and the test chat (which reads the saved map). @type {Snapshot | undefined} */
let live;
/** How far this installation is from being a working member of the account (metadata only). @type {Readiness | null} */
let readiness = null;
/** The section on screen. @type {Section} */
let tab = 'overview';
/** Whether the forms have unsaved edits. @type {boolean} */
let dirty = false;
/** The saved configuration and map the form was loaded from, to notice when they change underneath it. @type {string | null} */
let loadedProfile = null;
/** The test chat's conversation. @type {string} */
let session = crypto.randomUUID();

/** @param {any} value */
function setTab(value) { tab = value; }
/** @param {any} value */
function setDirty(value) { dirty = value; }
/** @param {any} value */
function setLive(value) { live = value; }
/** @param {any} value */
function setCurrent(value) { current = value; }
/** @param {any} value */
function setLoadedProfile(value) { loadedProfile = value; }
/** @param {any} value */
function setReadiness(value) { readiness = value; }
/** @param {any} value */
function setSession(value) { session = value; }

export {
  current, live, readiness, tab, dirty, loadedProfile, session, setCurrent, setDirty, setLive, setLoadedProfile,
  setReadiness, setSession, setTab,
};
