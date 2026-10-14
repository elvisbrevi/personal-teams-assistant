const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../desktop/ui/app.js'), 'utf8');
const i18n = fs.readFileSync(path.join(__dirname, '../desktop/ui/i18n.js'), 'utf8');
const invokeSource = source.slice(0, source.indexOf('\nconst $ ='));
const renderSource = source.slice(source.indexOf('const activityStates = {'), source.indexOf('\nfunction renderRegistrationSettings('));
/** A context with the interface strings in `language`, as a browser that stored it would load them. */
function localized(language, extra) {
  const context = vm.createContext({ localStorage: { getItem: () => language, setItem() {} }, navigator: { language: 'en-US' }, Intl, ...extra });
  vm.runInContext(i18n, context);
  return context;
}
const entry = { id: '11111111-1111-4111-8111-111111111111' };

test('other-day activity needs an explicit date explanation before creation', () => {
  const nodes = Object.fromEntries(Object.entries({
    '#activity-date-attribution': '', '#activity-parent': '40', '#activity-hours': '1',
    '#activity-title': 'Document the checklist', '#activity-description': 'Published the checklist.',
  }).map(([id, value]) => [id, { value }]));
  const context = localized('es', {
    $: id => nodes[id], requiredInputs: [], activitySummary: null,
    viewedActivity: { day: '2026-10-13', activities: [{ date: '2026-10-14' }] },
  });
  vm.runInContext(source.slice(source.indexOf('function activityFormProblem()'), source.indexOf('\nfunction showActivityProblem(')), context);
  const problem = vm.runInContext('activityFormProblem()', context);
  assert.match(problem[0], /Explica por qué este trabajo corresponde al día elegido/);
  assert.equal(problem[1], '#activity-date-attribution');
  nodes['#activity-date-attribution'].value = 'Worked before midnight; published afterward.';
  assert.equal(vm.runInContext('activityFormProblem()', context), null);
  nodes['#activity-date-attribution'].value = '';
  context.viewedActivity.activities[0].date = '2026-10-13';
  assert.equal(vm.runInContext('activityFormProblem()', context), null);
});

test('browser review opens the selected activity without a native window request', async () => {
  const calls = [], rendered = [], tabs = [];
  const context = vm.createContext({
    window: { ptaTransport: { browser: true, invoke: async (_command, request) => {
      calls.push(request);
      return { ok: false, code: 'not_ready', message: 'This host runs without a window; operate it with pta.' };
    } } },
    current: { revision: 'saved' }, pendingActivities: [entry], selectedActivity: null,
    activityEditing: true, activityPayload: 'old',
    renderActivityEditor: value => rendered.push(value.id),
    showTab: (...args) => tabs.push(args),
  });
  vm.runInContext(invokeSource, context);
  for (const args of [{ id: entry.id }, {}]) {
    const result = await vm.runInContext(`invoke('activity_open', ${JSON.stringify(args)})`, context);
    assert.equal(result.id, entry.id);
    assert.equal(context.selectedActivity, entry.id);
    assert.equal(context.activityEditing, false);
    assert.equal(context.activityPayload, '');
  }
  assert.deepEqual(calls, []);
  assert.deepEqual(rendered, [entry.id, entry.id]);
  assert.deepEqual(tabs, [['registration', 'activity-editor'], ['registration', 'activity-editor']]);
});

test('native review still delegates to the host window', async () => {
  const calls = [];
  const context = vm.createContext({
    window: { ptaTransport: { browser: false, invoke: async (_command, request) => {
      calls.push(request.request);
      return { ok: true, data: { visible: true } };
    } } },
    current: { revision: 'saved' },
  });
  vm.runInContext(invokeSource, context);
  const result = await vm.runInContext(`invoke('activity_open', { id: '${entry.id}' })`, context);
  assert.equal(result.visible, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].method, 'activity_open');
  assert.equal(calls[0].args.id, entry.id);
});

test('run cards render absent results and completed effort totals', () => {
  const context = localized('en', {
    el: (tag, props, ...children) => ({ tag, props, children }), badge: () => ({}),
  });
  vm.runInContext(renderSource, context);
  for (const status of ['running', 'interrupted', 'failed', 'completed']) {
    const summary = status === 'completed'
      ? { created: 0, pending: 3, recorded_hours: 1.5, remaining_hours: 7.5, warnings: [] }
      : null;
    const run = { id: entry.id, day: '2026-10-13', created_at: 1791931969, status, summary, diagnostics: {} };
    context.run = run;
    const card = vm.runInContext('renderActivityRun(run)', context);
    if (summary) assert.match(JSON.stringify(card), /1\.50 hours accounted for.*7\.50 hours remaining/);
    else assert.doesNotMatch(JSON.stringify(card), /hours accounted for/);
  }
});

test('run cards follow the interface language', () => {
  const context = localized('es', {
    el: (tag, props, ...children) => ({ tag, props, children }), badge: (text, tone) => ({ text, tone }),
  });
  vm.runInContext(renderSource, context);
  context.run = { id: entry.id, day: '2026-10-13', created_at: 1791931969, status: 'completed',
    summary: { created: 1, pending: 2, recorded_hours: 1.5, remaining_hours: 7.5, warnings: ['Source wiki has incomplete coverage.'] },
    diagnostics: { stage: 'Saving review results', finished_at: 1791932000 } };
  const card = JSON.stringify(vm.runInContext('renderActivityRun(run)', context));
  assert.match(card, /1,50 horas contabilizadas · faltan 7,50 horas/);
  assert.match(card, /Guardando los resultados de la revisión/);
  assert.match(card, /La fuente wiki tiene cobertura incompleta\./);
  assert.match(card, /"text":"Completada","tone":"ok"/);
});
