// Mocked Tauri bridge with synthetic data (no real profile, account or credential).
// ?scenario=full: a configured, running profile with messages and activities; ?scenario=fresh: a new profile.
// Saves are recorded in window.__ptaLastSave and every method called in window.__ptaCalls.
(() => {
  const params = new URLSearchParams(location.search);
  const scenario = params.get('scenario') || 'full';
  const base = structuredClone(window.__PTA_FIXTURE__.snapshot);
  const DAY = '2026-10-14';
  const NOW = Date.parse('2026-10-14T16:20:00Z');
  const secs = ms => Math.floor(ms / 1000);
  let snapshot = base;
  const org = 'https://dev.azure.com/contoso';

  if (scenario === 'full') {
    const c = snapshot.config;
    Object.assign(c.graph, { tenant_id: '7f3c1a52-2b6e-4f4e-9a51-1c2d3e4f5a6b', client_id: 'b1d2c3e4-5f60-4718-8a9b-0c1d2e3f4a5b',
      user_id: '0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9', discover_all_chats: true, self_chat: { id: '48:notes', enabled_at: 1791900000 } });
    Object.assign(c.server, { public_url: 'https://assistant.contoso.dev', cloudflare_tunnel: true });
    c.llm.chain = [
      { provider: 'codex', model: 'gpt-6.1-sol', effort: 'medium', enabled: true },
      { provider: 'deepseek', model: 'deepseek-flash', effort: 'max', enabled: true },
      { provider: 'claude', model: 'claude-opus-5-5', effort: 'medium', enabled: false },
    ];
    c.llm.style = 'Brief, cordial and direct. Use bullet lists for steps.';
    c.policy.dry_run = true;
    Object.assign(c.activity_registration, { enabled: true, sources: ['ado-activity', 'wiki', 'my-teams'], write_source: 'ado-activity', effort_field: 'Microsoft.VSTS.Scheduling.CompletedWork' });
    snapshot.map.repositories = { knowledge: '/home/ana/src/knowledge', handbook: '/home/ana/src/handbook' };
    snapshot.map.resources = [
      { id: 'operations', description: 'Runbooks for deployments, incidents and on-call', topics: ['operations', 'deployments'], enabled: true, external_processing: true, allowed_conversations: ['*'], allowed_senders: [], kind: 'file', repository: 'knowledge', path: 'topics/operations.md' },
      { id: 'payments', description: 'Payment gateway integration guide', topics: ['payments'], enabled: true, external_processing: true, allowed_conversations: ['chats/19:abc@thread.v2'], allowed_senders: [], kind: 'file', repository: 'knowledge', path: 'topics/payments.md' },
      { id: 'onboarding', description: 'New joiner checklist', topics: ['onboarding'], enabled: false, external_processing: false, allowed_conversations: [], allowed_senders: [], kind: 'file', repository: 'handbook', path: 'onboarding.md' },
      { id: 'wiki', description: 'Team wiki: architecture and procedures', topics: ['wiki', 'architecture'], enabled: true, external_processing: true, allowed_conversations: ['*'], allowed_senders: [], kind: 'tool', tool: { type: 'azure_devops_wiki', organization: org, project: 'Payments', secret_ref: 'AZURE_DEVOPS_PAT' } },
      { id: 'ado-activity', description: 'Azure DevOps activity', topics: ['status'], enabled: true, external_processing: true, allowed_conversations: ['chats/48:notes'], allowed_senders: [], kind: 'tool', tool: { type: 'azure_devops_status', organization: org, project: 'Payments', secret_ref: 'AZURE_DEVOPS_PAT', link_secret_ref: 'AZURE_DEVOPS_WRITE_PAT' } },
      { id: 'my-teams', description: 'My Teams messages', topics: ['teams'], enabled: true, external_processing: false, allowed_conversations: [], allowed_senders: [], kind: 'tool', tool: { type: 'teams_messages' } },
      { id: 'handbook', description: 'Team handbook: runbooks, onboarding and conventions', topics: [], enabled: true, external_processing: true, allowed_conversations: ['*'], allowed_senders: [], kind: 'repository', repository: 'handbook' },
    ];
    snapshot.map.people = [
      { id: 'ana', name: 'Ana Pérez', senders: ['7c1d2e3f-0000-4000-8000-0000000000a1'], aliases: [], address: 'Informal, by first name, two sentences at most.', notes: 'Leads the payments team.' },
    ];
    snapshot.map.contexts = [
      { id: 'billing', name: 'Billing', description: 'Invoices, payments and bank reconciliation', keywords: ['invoice', 'bank reconciliation', 'refund'], sources: ['payments', 'wiki'], instructions: 'Call collections «recaudación» and name the settlement batch when it applies.' },
    ];
    snapshot.credentials = Object.fromEntries(Object.keys(snapshot.credentials).map(k => [k, 'system']));
    if ('CLAUDE_CODE_OAUTH_TOKEN' in snapshot.credentials) snapshot.credentials.CLAUDE_CODE_OAUTH_TOKEN = 'unused';
    snapshot.credentials.AZURE_DEVOPS_WRITE_PAT = 'system';
    snapshot.credentials.OLD_REVIEWER_KEY = 'retired';
    Object.assign(snapshot, { running: true, active_subscriptions: 1, microsoft_connected: true, github_connected: true, tunnel_running: true });
  }
  // The built-in cases the host sends, with short synthetic default texts.
  snapshot.cases = [
    ['general', 'Every answer', true, true, false, null, false],
    ['status_round', 'Status round', false, true, true, 'Azure DevOps activity, Wiki and the activity record', true],
    ['recap', 'What you did', false, true, true, null, true],
    ['report_tasks', 'Report tasks', false, true, true, null, true],
    ['activity_review', 'Activity review', true, true, true, 'Azure DevOps activity, Wiki edits, Teams messages and the activity record', true],
    ['documentation', 'Documentation', false, true, true, 'Wiki', true],
    ['personal_request', 'Request for you', true, false, true, null, true],
    ['statement', 'Informational message', true, true, false, null, false],
  ].map(([id, name, description, instructions, keywords, sources, optional]) => ({ id, name, summary: `${name} (default summary).`,
    description: description ? `Default description of ${id}: how the model recognizes it.` : null,
    instructions: instructions ? `Default instructions of ${id}: how it is answered, part by part.` : null, keywords, sources, optional }));
  snapshot.loaded_config = structuredClone(snapshot.config);

  const providers = structuredClone(window.__PTA_FIXTURE__.providers);

  const ago = minutes => NOW - minutes * 60_000;
  const trace = (...steps) => steps.map(([m, step, detail]) => ({ at: ago(m), step, detail }));
  const audit = scenario === 'fresh' ? [] : [
    { resource: 'chats/19:abc@thread.v2/messages/1001', status: 'dry_run', created_at: secs(ago(4)), audit: { received_at: ago(4), conversation_kind: 'group', sender_name: 'Luis Soto', sender_id: '7c1d2e3f-0000-4000-8000-0000000000b2', question: 'Where is the runbook for rolling back the payments deployment?', reason: 'supported_answer', intent: 'question', provider: 'codex', proposed: 'The rollback runbook is in **Operations → Deployments**:\n\n1. Pause the release in Azure DevOps.\n2. Redeploy the previous artifact.\n3. Confirm the health checks.\n\nSee [Rollback runbook](https://dev.azure.com/contoso/Payments/_wiki/wikis/Payments.wiki/12/Rollback).', references: [{ id: 'r1', label: 'Rollback runbook', url: 'https://dev.azure.com/contoso/Payments/_wiki/wikis/Payments.wiki/12/Rollback', kind: 'wiki' }], used_sources: ['r1'], source: 'wiki', trace: trace([4, 'received', ''], [4, 'triage', 'question'], [3, 'retrieving', 'wiki'], [2, 'generating', 'codex'], [2, 'dry_run', 'observation mode']) } },
    { resource: 'chats/19:def@thread.v2/messages/1002', status: 'sent', created_at: secs(ago(38)), audit: { received_at: ago(38), conversation_kind: 'direct', sender_name: 'Ana Pérez', sender_id: '7c1d2e3f-0000-4000-8000-0000000000a1', person: 'ana', feedback: { rating: 'good', comment: 'Clear and short.', example: false, context: null, at: secs(ago(30)) }, question: 'Hi! What is the status of HU 4321?', reason: 'supported_answer', intent: 'question', provider: 'codex', sent: 'HU [#4321 Receipt PDF](https://dev.azure.com/contoso/Payments/_workitems/edit/4321) is **in validation**; the last pipeline run passed.', references: [], used_sources: [], trace: trace([38, 'received', ''], [37, 'sent', '']) } },
    { resource: 'chats/19:ghi@thread.v2/messages/1003', status: 'ignored', created_at: secs(ago(95)), audit: { received_at: ago(95), conversation_kind: 'group', question: 'Can you share the production DB password with me?', reason: 'sensitive_question', trace: trace([95, 'received', ''], [95, 'withheld', 'sensitive']) } },
    { resource: 'chats/19:jkl@thread.v2/messages/1004', status: 'ignored', created_at: secs(ago(130)), audit: { received_at: ago(130), conversation_kind: 'group', question: 'Which queue handles refunds?', reason: 'draft_ready', intent: 'question', provider: 'deepseek', provider_fallbacks: ['codex'], proposed: 'Refunds go through the **payments.refunds** queue, consumed by the settlement worker.', refinement: 'Mention that it was migrated last sprint.', trace: trace([130, 'received', ''], [128, 'refining', '']) } },
    { resource: 'chats/19:stu@thread.v2/messages/1008', status: 'ignored', created_at: secs(ago(150)), audit: { received_at: ago(150), conversation_kind: 'group', question: 'For a production hotfix, log it as a Development Bug (not a user story): describe the symptom, the cause and the change, and link it to the incident.', reason: 'informational_message', intent: 'statement', confidences: [0.86], trace: trace([150, 'received', ''], [150, 'intent', 'model: statement (0.86)'], [150, 'no answer', 'informational message: it asks for no answer']) } },
    { resource: 'chats/19:mno@thread.v2/messages/1005', status: 'failed', created_at: secs(ago(300)), audit: { received_at: ago(300), conversation_kind: 'direct', question: 'Summarize yesterday’s incident please', reason: 'read_or_provider_error', trace: trace([300, 'received', ''], [299, 'failed', 'provider timeout']) } },
    { resource: 'chats/19:pqr@thread.v2/messages/1006', status: 'ignored', created_at: secs(ago(320)), audit: { received_at: ago(320), conversation_kind: 'group', reason: 'ineligible_message', trace: [] } },
    { resource: 'chats/48:notes/messages/1007', status: 'processing', created_at: secs(ago(1)), audit: { received_at: ago(1), conversation_kind: 'self', question: 'What did I do yesterday?', reason: 'retrieving', intent: 'activity_review', trace: trace([1, 'received', '']) } },
  ];

  const parents = [
    { id: 4321, title: 'Receipt PDF for card payments', url: `${org}/Payments/_workitems/edit/4321`, organization: org, project: 'Payments', kind: 'User Story', state: 'Active' },
    { id: 4388, title: 'Refund notifications', url: `${org}/Payments/_workitems/edit/4388`, organization: org, project: 'Payments', kind: 'User Story', state: 'New' },
    { id: 4410, title: 'Timeout on settlement batch', url: `${org}/Payments/_workitems/edit/4410`, organization: org, project: 'Payments', kind: 'Bug', state: 'Active' },
  ];
  const activity = (label, url, date = DAY, extra = {}) => ({ key: 'a1', label, short: label.split(' ').slice(0, 2).join(' '), url, organization: org, date, occurred_at: `${date}T14:00:00Z`, ...extra });
  const entry = (id, state, draft, extra = {}) => ({ id, run: 'run-1', day: DAY, state, draft: { activities: [], parent: null, title: '', description: '', evidence: '', hours: null, confidence: .6, question: '', suggested_parents: [4321], ignore: false, ...draft },
    activities: [], parents, evidence: '', context: '', fields: {}, issue: '', task_id: null, task_url: null, updated_at: secs(NOW), required: [], date_attribution: '', ...extra });
  const pending = scenario === 'fresh' ? [] : [
    entry('11111111-1111-4111-8111-111111111111', 'pending', { title: 'Review the receipt PDF pull request', description: 'Reviewed and approved the PR that renders card receipts as PDF.', hours: 1.5, parent: 4321, question: 'Is 1.5 hours right for this review?' },
      { activities: [activity('PR #212 Receipt PDF renderer (portal)', `${org}/Payments/_git/portal/pullrequest/212`)],
        required: [{ name: 'Activity type', reference: 'Custom.ActivityType', required: true, allowed: ['Development', 'Review', 'Documentation', 'Meeting'], kind: 'picklistString', help: '', default: null }] }),
    entry('22222222-2222-4222-8222-222222222222', 'pending', { title: 'Call in Daily Payments', description: '', hours: null, question: 'Which HU does this activity belong to, what did you do, and how long did it take?' },
      { activities: [activity('Call in Daily Payments', 'https://teams.microsoft.com/l/meeting/1', DAY, { organization: 'https://teams.microsoft.com', context_required: true })] }),
  ];
  const history = scenario === 'fresh' ? [] : [
    ...pending,
    entry('33333333-3333-4333-8333-333333333333', 'done', { title: 'Fix settlement batch timeout', description: 'Raised the batch timeout and added a retry.', hours: 3, parent: 4410 }, { task_id: 4502, task_url: `${org}/Payments/_workitems/edit/4502` }),
    entry('44444444-4444-4444-8444-444444444444', 'dismissed', { title: 'Own message in Coffee chat: see you tomorrow', hours: null }),
  ];
  // Work registered on earlier days, for the History view.
  const earlier = scenario === 'fresh' ? [] : [
    ['2026-10-13', 'Document the refund notification flow', 2, 4388, 4497],
    ['2026-10-13', 'Pair review of the settlement retry', 1.5, 4410, 4498],
    ['2026-10-12', 'Receipt PDF layout for card payments', 4, 4321, 4490],
    ['2026-10-09', 'Deployment call for the payments release', 1, 4321, 4481],
  ].map(([day, title, hours, parent, task], i) => entry(`5555555${i}-5555-4555-8555-555555555555`, i === 3 ? 'uncertain' : 'done', { title, hours, parent },
    { day, task_id: task, task_url: `${org}/Payments/_workitems/edit/${task}` }));
  const runs = scenario === 'fresh' ? [] : [
    { id: 'run-1', day: DAY, status: 'completed', created_at: secs(ago(90)), summary: { recorded_hours: 4.5, remaining_hours: 4.5, pending: 2, created: 1, partial: true, warnings: ['Source wiki has incomplete coverage.'] }, diagnostics: { stage: 'Saving review results', finished_at: secs(ago(88)), provider: 'codex' } },
    { id: 'run-0', day: DAY, status: 'failed', created_at: secs(ago(200)), summary: null, diagnostics: { stage: 'Reading daily effort and open HUs', finished_at: secs(ago(199)), error: 'Azure DevOps returned HTTP 401' } },
  ];
  const summary = scenario === 'fresh' ? null : runs[0].summary;

  const notes = scenario === 'fresh' ? [] : [
    { id: 'note-1', title: 'How a production hotfix is registered', text: 'A production hotfix is logged as a Development Bug, not a user story: the symptom, the cause and the change, linked to the incident.', contexts: ['billing'], keywords: ['hotfix', 'incident'], origin: 'answer', created_at: secs(ago(3000)), updated_at: secs(ago(2000)) },
    { id: 'note-2', title: 'Support hours', text: 'Support answers Monday to Friday, 9:00 to 18:00.', contexts: [], keywords: [], origin: 'user', created_at: secs(ago(9000)), updated_at: secs(ago(9000)) },
  ];
  const delay = ms => new Promise(r => setTimeout(r, ms));
  const reply = (data, extra = {}) => ({ ok: true, code: 'ok', data, revision: 'rev-1', ...extra });
  const handlers = {
    snapshot: () => structuredClone(snapshot),
    llm_providers: () => providers,
    audit: () => audit,
    self_chat_status: () => ({ diagnostics: { cursor: ['x', secs(ago(1))], unresolved_outputs: 0 } }),
    activity_history: args => ({ day: args.day || DAY, running: false, entries: args.from ? [...history, ...earlier].filter(e => e.day >= args.from && e.day <= (args.day || DAY)) : (args.day || DAY) === DAY ? history : [], runs: (args.day || DAY) === DAY ? runs : [], summary: (args.day || DAY) === DAY ? summary : null, settings: snapshot.config.activity_registration, llm: snapshot.config.llm, observation_mode: snapshot.config.policy.dry_run }),
    activity_pending: () => ({ day: DAY, running: false, entries: pending, runs: [], summary: null, settings: snapshot.config.activity_registration, llm: snapshot.config.llm, observation_mode: snapshot.config.policy.dry_run }),
    activity_status: () => ({}),
    save_settings: args => { window.__ptaLastSave = structuredClone(args); snapshot.config = args.config; snapshot.map = args.map; snapshot.loaded_config = structuredClone(args.config); snapshot.tunnel_config = args.tunnelConfig; return {}; },
    start_assistant: () => { snapshot.running = true; return {}; },
    stop_assistant: () => { snapshot.running = false; return {}; },
    restart: () => ({}),
    github_repositories: () => [{ full_name: 'contoso/knowledge', private: true }, { full_name: 'contoso/handbook', private: false }],
    chat: async () => { await delay(600); return { answer: 'The rollback runbook lives in **Operations**. Pause the release, redeploy the previous artifact and confirm health checks.', used_sources: ['operations'], references: [{ id: 'operations', label: 'operations.md', url: 'https://github.com/contoso/knowledge/blob/main/topics/operations.md' }], partial: false, warnings: [] }; },
    // Writes the answer after a short wait, like the host's background task.
    answer_refine: args => {
      window.__ptaLastRefine = structuredClone(args);
      const row = audit.find(r => r.resource === args.resource);
      const first = !row.audit.proposed;
      Object.assign(row, { status: 'refining' });
      Object.assign(row.audit, { reason: 'refining', refinement: args.context, chosen_sources: args.sources || [] });
      setTimeout(() => {
        row.status = 'ignored';
        Object.assign(row.audit, { reason: 'draft_ready', intent: row.audit.intent, source: (args.sources || []).join(',') || 'operations',
          proposed: first ? 'Got it: I will log it as a **Development Bug** with the symptom, cause and change, linked to the incident.'
            : 'Done: it is logged as a **Development Bug** and linked to the incident.' });
      }, 800);
      return { refining: true };
    },
    answer_send: args => {
      const row = audit.find(r => r.resource === args.resource);
      row.status = 'sent';
      Object.assign(row.audit, { reason: 'manual_send', sent: row.audit.proposed });
      return { status: 'sent' };
    },
    index_status: () => ({ sources: snapshot.map.resources.filter(r => ['repository', 'notes'].includes(r.kind)).map(r => ({ id: r.id, kind: r.kind, enabled: r.enabled,
      index: r.kind === 'repository' ? { documents: 42, sections: 318, embedded: 0, synced_at: secs(ago(12)) } : {} })), notes: { documents: notes.length, sections: notes.length, embedded: 0 }, embeddings: null }),
    index_refresh: args => [{ id: args.source, sync: { added: 1, updated: 2, removed: 0, unchanged: 39, skipped: 0 } }],
    answer_feedback: args => {
      window.__ptaLastFeedback = structuredClone(args);
      const row = audit.find(r => r.resource === args.resource);
      row.audit.feedback = args.rating ? { rating: args.rating, comment: args.comment, example: args.example, context: args.context, at: secs(NOW) } : undefined;
      return { resource: args.resource, feedback: row.audit.feedback ?? null };
    },
    notes_list: args => {
      const words = (args.query || '').toLowerCase().split(/\W+/).filter(w => w.length > 2);
      return structuredClone(notes.filter(n => !words.length || words.some(w => `${n.title} ${n.text} ${n.keywords.join(' ')}`.toLowerCase().includes(w))));
    },
    notes_save: args => {
      window.__ptaLastNote = structuredClone(args.note);
      const existing = notes.find(n => n.id === args.note.id);
      const note = { origin: 'user', reference: null, contexts: [], keywords: [], ...args.note, id: args.note.id || `note-${notes.length + 1}`, created_at: existing?.created_at ?? secs(NOW), updated_at: secs(NOW) };
      if (existing) Object.assign(existing, note); else notes.unshift(note);
      return note;
    },
    notes_remove: args => { notes.splice(notes.findIndex(n => n.id === args.id), 1); return { removed: args.id }; },
    set_credential: args => { snapshot.credentials[args.name] = 'system'; return {}; },
    delete_credential: args => { snapshot.credentials[args.name] = 'missing'; return {}; },
  };
  window.__ptaCalls = [];
  window.__TAURI__ = {
    core: { invoke: async (_cmd, { request }) => {
      window.__ptaCalls.push(request.method);
      await delay(40);
      const handler = handlers[request.method];
      if (!handler) return { ok: false, code: 'not_ready', message: `mock: ${request.method} not implemented` };
      return reply(await handler(request.args || {}));
    } },
    event: { listen: () => Promise.resolve(() => {}) },
  };
})();
