// A workflow run's states as the person reads them, and what a workflow lets its coding agent do
// (`allow`): one wording for Home, the Implement form, the workflow editor and whatever shows a
// run next.
import { t, tx } from './i18n.js';

/**
 * What a workflow may let its coding agent do that the fixed rules otherwise forbid (`allow`),
 * in the host's order, with what each box authorizes.
 */
const ALLOWS = [
  { id: 'work_items', name: () => t('Change work items'), effect: () => t('Create and change Azure DevOps and GitHub work items and issues: their state, fields and comments.') },
  { id: 'teams', name: () => t('Send Teams messages'), effect: () => t('Post to Teams chats as you, for example with pta teams send.') },
  { id: 'merge', name: () => t('Merge pull requests'), effect: () => t('Complete and merge pull requests, its own included.') },
  { id: 'deploy', name: () => t('Publish and deploy'), effect: () => t('Create releases, run deployment pipelines and publish packages.') },
];
/** The names of what a workflow allows, in the host's order. */
function allowNames(allow) {
  return ALLOWS.filter(a => (allow ?? []).includes(a.id)).map(a => a.name());
}

const LABELS = {
  classifying: () => t('Choosing a workflow'),
  unassigned: () => t('Needs a workflow'),
  planning: () => t('Planning'),
  plan_review: () => t('Plan to review'),
  creating_children: () => t('Creating the children'),
  coding: () => t('Coding'),
  pull_requests: () => t('Waiting for pull requests'),
  blocks: () => t('Running the workflow\'s blocks'),
  done: () => t('Done'),
  failed: () => t('Failed'),
  cancelled: () => t('Cancelled'),
  interrupted: () => t('Interrupted'),
  needs_attention: () => t('Needs attention'),
};

/** A run state in words (an unknown one is shown as the host spelled it). */
function runStateLabel(state) {
  return (LABELS[state] ?? (() => tx(state)))();
}

export { ALLOWS, allowNames, runStateLabel };
