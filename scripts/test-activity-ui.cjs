const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../desktop/ui/app.js'), 'utf8');
const invokeSource = source.slice(0, source.indexOf('\nconst $ ='));
const renderSource = source.slice(source.indexOf('function activityFailureHelp('), source.indexOf('\nfunction renderRegistrationSettings('));
const entry = { id: '11111111-1111-4111-8111-111111111111' };

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
  const context = vm.createContext({
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
