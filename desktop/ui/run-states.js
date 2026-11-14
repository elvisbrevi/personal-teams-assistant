// A workflow run's states as the person reads them, what a workflow lets its coding agent do
// (`allow`) and why an agent session did not end with its answer (`failure`): one wording for
// Home, the Implement form, Agent history, the workflow editor and whatever shows a run next.
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

/** Why an agent session did not end with its answer (`failure.cause`), with the next action. */
const CAUSES = {
  cli_missing: () => t('The selected CLI is not installed on the machine. Install it there, sign in and start again.'),
  model_unavailable: () => t('The machine\'s CLI does not offer the selected model or effort. Choose one it offers and start again.'),
  output_limit: () => t('The agent\'s final report was too large to read. Read the session for its report.'),
  timeout: () => t('The session reached its time limit and was stopped. Read the session for what it left unfinished.'),
  cancelled: () => t('The run was cancelled.'),
  permission_prompt: () => t('The agent stopped on a permission prompt nobody could answer. Read the recorded prompt, remove its cause and start again.'),
  invalid_answer: () => t('The agent\'s answer was not the JSON it was asked for. Try again, or choose another model.'),
  agent_error: () => t('The CLI reported an error for the agent\'s turn (for example a model, quota or service error). Read the session, then start again.'),
  no_report: () => t('The agent ended without a final report. Read the session for what it did.'),
  host_restarted: () => t('The application on the machine restarted while the session ran. Start the run again.'),
  machine_offline: () => t('The machine did not answer. Check that it is online and up to date, then try again.'),
  unknown: () => t('The machine did not say why (its version may predate causes). Update it and read the run\'s record and session there.'),
};
/** What the first stderr line of a CLI that exited at once said (`failure.stderr`). */
const STDERR = {
  missing_variable: () => t('It reported a missing system variable: update the application on that machine and start again.'),
  not_signed_in: () => t('It reported that it is not signed in: sign in to the CLI on that machine and start again.'),
  unknown_model: () => t('It reported an unknown model: choose a model it offers and start again.'),
  unknown_flag: () => t('It rejected an option it was given: update the CLI on that machine and start again.'),
  other: () => t('Run the CLI in a terminal on that machine to see why, then start again.'),
};
/**
 * Why an agent run or session did not end with its answer, in words with the next action; empty
 * without a failure. The host sends identifiers only, never what the CLI wrote.
 * @param {{ cause?: string, exit_code?: number, stderr?: string } | null | undefined} failure
 */
function failureText(failure) {
  if (!failure?.cause) return '';
  const code = Number.isInteger(failure.exit_code) ? String(failure.exit_code) : null;
  if (failure.cause === 'cli_exited_at_start') {
    const exited = code ? t('The CLI exited as soon as it started with exit code {code}.', { code }) : t('The CLI exited as soon as it started.');
    return `${exited} ${(STDERR[failure.stderr ?? 'other'] ?? STDERR.other)()}`;
  }
  if (failure.cause === 'stream_ended') {
    return code ? t('The CLI stopped with exit code {code} before its session finished. Read the session for what it did, then start again.', { code })
      : t('The CLI stopped before its session finished. Read the session for what it did, then start again.');
  }
  return (CAUSES[failure.cause] ?? CAUSES.unknown)();
}

export { ALLOWS, allowNames, failureText, runStateLabel };
