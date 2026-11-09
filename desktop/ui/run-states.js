// A workflow run's states as the person reads them: one wording for Home, the Implement form and
// whatever shows a run next.
import { t, tx } from './i18n.js';

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

export { runStateLabel };
