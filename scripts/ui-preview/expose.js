// The preview harness only: the GUI is ES modules, so nothing it declares is a global. The checks
// (check.mjs, shoot.mjs) reach a few of its functions and bindings inside the page; this module
// imports the same module instances the page runs and puts them on `window`. Bindings are getters,
// so a check reads the module's current value. Production pages never load this file.
import { transport } from '/transport.js';
import { invoke } from '/control.js';
import { $ } from '/dom.js';
import * as state from '/state.js';
import { applyAppearance } from '/appearance.js';
import { markDirty, selectTab, showTab } from '/nav.js';
import { todoRich } from '/markdown.js';
import { showMobileSession } from '/sessions.js';
import { loadMachines } from '/settings-machines.js';
import { loadAzureCliStatus, renderSteps } from '/home.js';
import { pollLive, refreshLive } from '/profile.js';
import * as account from '/settings-account.js';
import { loadConsumers, loadVariables } from '/settings-variables.js';
import { loadUpdates } from '/settings-updates.js';
import * as cloud from '/settings-cloud.js';
import * as models from '/settings-models.js';

window.ptaTransport = transport;
Object.assign(window, {
  invoke, $, applyAppearance, markDirty, selectTab, showTab, todoRich, showMobileSession, loadMachines,
  loadAzureCliStatus, renderSteps, pollLive, refreshLive, loadConsumers, loadVariables, loadUpdates,
  loadAgentEndpoint: account.loadAgentEndpoint, loadReadiness: account.loadReadiness, leaveAccount: account.leaveAccount,
  loadAccountConfig: account.loadAccountConfig, adoptAccount: account.adoptAccount, loadCloud: cloud.loadCloud,
  renderChain: models.renderChain,
});
for (const [name, module] of [['tab', state], ['readiness', state], ['live', state], ['accountEpoch', account],
  ['cloudStatus', cloud], ['llmRows', models], ['llmCatalog', models]]) {
  Object.defineProperty(window, name, { configurable: true, get: () => module[name] });
}
