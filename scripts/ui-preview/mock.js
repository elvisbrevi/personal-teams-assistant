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
    Object.assign(c.activity_registration, { enabled: true, sources: ['ado-activity'], write_source: '', effort_field: 'Microsoft.VSTS.Scheduling.CompletedWork' });
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
  const taskDays = scenario === 'fresh' ? [] : [
    [DAY, [[4502, 'Fix settlement batch timeout', 3, 4410]]],
    ['2026-10-13', [[4497, 'Document the refund notification flow', 2, 4388], [4498, 'Pair review of the settlement retry', 1.5, 4410]]],
    ['2026-10-12', [[4490, 'Receipt PDF layout for card payments', 4, 4321]]],
    ['2026-10-09', [[4481, 'Deployment call for the payments release', 1, 4321]]],
    ['2026-10-08', []],
  ].map(([day, tasks]) => ({ day, time_zone: 'America/Santiago', target_hours: 9, saved_at: secs(NOW), partial: false, warnings: [],
    tasks: tasks.map(([id, title, hours, parent]) => ({ item: { id, title, url: `${org}/Payments/_workitems/edit/${id}`, project: 'Payments', kind: 'Task', state: 'Done', created_at: `${day}T12:00:00Z`, dates: [] },
      source: 'ado-activity', organization: org, user: 'ana@example.test', description: title, hours, parent: { ...parents.find(p => p.id === parent), created_at: null, dates: [] } })),
    hours: tasks.reduce((sum, t) => sum + t[2], 0) }));

  const notes = scenario === 'fresh' ? [] : [
    { id: 'note-1', title: 'How a production hotfix is registered', text: 'A production hotfix is logged as a Development Bug, not a user story: the symptom, the cause and the change, linked to the incident.', contexts: ['billing'], keywords: ['hotfix', 'incident'], origin: 'answer', created_at: secs(ago(3000)), updated_at: secs(ago(2000)) },
    { id: 'note-2', title: 'Support hours', text: 'Support answers Monday to Friday, 9:00 to 18:00.', contexts: [], keywords: [], origin: 'user', created_at: secs(ago(9000)), updated_at: secs(ago(9000)) },
  ];
  const delay = ms => new Promise(r => setTimeout(r, ms));
  const reply = (data, extra = {}) => ({ ok: true, code: 'ok', data, revision: 'rev-1', ...extra });
  // Only trusted synthetic HTML is parsed here; real provider HTML is parsed by the Rust core.
  function richFixture(text) {
    const node = element => element.nodeType === 3 ? { tag: 'text', text: element.textContent, children: [] } : {
      tag: element.tagName.toLowerCase(), text: '', url: element.getAttribute('href'), children: [...element.childNodes].map(node),
    };
    return [...new DOMParser().parseFromString(text, 'text/html').body.childNodes].map(node);
  }
  async function todoDetail(args, github) {
    window.__ptaLastDetail = { provider: github ? 'github' : 'azure', ...structuredClone(args) };
    await delay(window.__ptaDetailDelay || 0);
    if (window.__ptaDetailError) throw new Error('Azure HU read timed out.');
    const bug = !github && args.id === 4999;
    const field = (reference, name, kind, value) => ({ reference, name, kind, value, content: kind === 'html' ? richFixture(value) : null });
    return {
      item: { id: args.id, title: github ? 'Billing platform improvements' : bug ? 'Receipt download times out' : 'Implement receipt downloads', project: args.project,
        kind: github ? 'Epic' : bug ? 'Bug' : 'User Story', state: 'Developing', created_at: '2026-10-01T12:00:00Z', dates: [{ name: 'Development start', value: '2026-10-12T12:00:00Z' }],
        url: github ? `https://github.com/${args.project}/issues/${args.id}` : `${org}/${args.project}/_workitems/edit/${args.id}` },
      fields: [field(github ? 'assignees' : 'System.AssignedTo', 'Assigned To', 'string', 'Ana Pérez'),
        field(github ? 'body' : 'System.Description', 'Description', github ? 'markdown' : 'html', github ? '## Goal\nMake billing easier on a phone.' : '<p>Customers can download their <strong>payment receipt</strong> from the portal.</p><ul><li>Available after payment confirmation.</li><li>Keep the original receipt number.</li></ul>'),
        field(bug ? 'Microsoft.VSTS.TCM.ReproSteps' : 'Microsoft.VSTS.Common.AcceptanceCriteria', bug ? 'Repro Steps' : 'Acceptance Criteria', 'html', '<ol><li>Open a confirmed payment.</li><li>Download a readable PDF receipt.</li></ol>'),
        field('Custom.Evidence', 'Evidence', 'html', '<p>Validated in the test environment.</p><a href="https://dev.azure.com/contoso/Payments/_build/results?buildId=42">Validation run</a>'),
        field('Custom.Branch', 'Branch', 'string', 'feature/receipt-downloads'),
        field('Microsoft.VSTS.Common.Priority', 'Priority', 'integer', 2),
        ...(bug ? [field('Microsoft.VSTS.Common.Severity', 'Severity', 'string', '2 - High')] : []),
        ...(github ? [field('labels', 'Labels', 'string', 'payments, mobile'), field('milestone', 'Milestone', 'string', 'October')] : [field('System.IterationPath', 'Iteration Path', 'string', 'Payments / Sprint 12')])],
      comments: [{ author: 'Luis Soto', created_at: '2026-10-14T12:00:00Z', modified_at: null, text: '<p>Validated on a phone. The PDF is <strong>readable</strong>.</p>', content: richFixture('<p>Validated on a phone. The PDF is <strong>readable</strong>.</p>'), format: 'html' }],
      relations: [{ name: github ? 'Sub-issue' : 'Branch', url: github ? 'https://github.com/contoso/billing/issues/43' : `${org}/Payments/_git/portal?version=GBfeature%2Freceipt-downloads`, detail: github ? '#43 · Receipt downloads · open' : 'feature/receipt-downloads' },
        { name: 'Attachment', url: `${org}/_apis/wit/attachments/receipt`, detail: 'receipt-validation.png' }],
      history: [{ author: 'Ana Pérez', at: '2026-10-14T12:00:00Z', fields: [field('System.State', 'State', 'string', { oldValue: 'New', newValue: 'Developing' })] }],
      partial: !!window.__ptaDetailPartial, warnings: window.__ptaDetailPartial ? ['Comments could not be read. Open the original item or try again.'] : [],
    };
  }
  const handlers = {
    get_azure_task: async args => { const detail = await todoDetail(args, false); detail.item.kind = 'Task'; return detail; },
    activity_run: args => { window.__ptaLastActivityRun = args; return { running: true }; },
    get_azure_hu: args => todoDetail(args, false),
    get_github_epic: args => todoDetail(args, true),
    list_azure_hus: async args => {
      window.__ptaLastTodo = { provider: 'azure', ...structuredClone(args) };
      await delay(window.__ptaTodoDelay || 0);
      if (window.__ptaTodoError) throw new Error('Azure HU read timed out.');
      const items = Array.from({ length: 15 }, (_, index) => ({ id: 5000 - index, title: ['Implement receipt downloads', 'Review payment validation', 'Improve settlement reporting'][index % 3],
        url: `${org}/Payments/_workitems/edit/${5000 - index}`, project: 'Payments', kind: index % 3 === 1 ? 'Bug' : 'User Story', state: index % 2 ? 'Finished' : 'Developing',
        created_at: `2026-10-${String(19 - index).padStart(2, '0')}T12:00:00Z`,
        dates: [
          { field: 'Microsoft.VSTS.Scheduling.StartDate', name: 'Start date', value: '2026-10-01T12:00:00Z' },
          { field: 'Microsoft.VSTS.Scheduling.TargetDate', name: 'Completion date', value: '2026-11-30T12:00:00Z' },
          { field: 'Custom.DevStart', name: 'Development start', value: index % 2 ? null : '2026-10-12T12:00:00Z' },
          { field: 'Custom.DevFinish', name: 'Development finish', value: '2026-10-30T12:00:00Z' },
        ] }));
      const filtered = items.filter(item => (!args.state || item.state === args.state) && (!args.kind || (args.kind === 'bug') === (item.kind === 'Bug')));
      return { items: filtered.slice(0, args.limit), states: ['Developing', 'Finished', 'Awaiting validation'], projects: ['Payments'], assignment_fields: ['Developer 1', 'Developer 2'], partial: !!window.__ptaTodoPartial, warnings: [], has_more: filtered.length > args.limit };
    },
    list_github_epics: args => {
      window.__ptaLastTodo = { provider: 'github', ...structuredClone(args) };
      const items = [{ id: 42, title: 'Billing platform improvements', url: 'https://github.com/contoso/billing/issues/42', project: 'contoso/billing', kind: 'Epic', state: 'open', created_at: '2026-10-18T12:00:00Z',
        dates: [{ field: 'start_date', name: 'Start date', value: null }, { field: 'closed_at', name: 'Completion date', value: null }] }];
      return { items: items.filter(item => !args.state || item.state === args.state), states: ['open', 'closed'], projects: ['contoso/billing'], assignment_fields: ['Assignees'], partial: false, warnings: [], has_more: false };
    },
    open_work_item: args => { window.__ptaOpenedWorkItem = args.url; return { url: args.url }; },
    snapshot: () => structuredClone(snapshot),
    llm_providers: () => providers,
    audit: () => audit,
    self_chat_status: () => ({ diagnostics: { cursor: ['x', secs(ago(1))], unresolved_outputs: 0 } }),
    activity_history: args => ({ day: args.day || DAY, running: !!window.__ptaActivityRunning,
      days: taskDays.filter(d => (!args.from || d.day >= args.from) && d.day <= (args.day || DAY)).map(d => ({ ...d, tasks: d.tasks.length })),
      record: taskDays.find(d => d.day === (args.day || DAY)) || null, entries: [], runs: [], summary: null, settings: snapshot.config.activity_registration }),
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
