const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../desktop/ui/app.js'), 'utf8');
const i18n = fs.readFileSync(path.join(__dirname, '../desktop/ui/i18n.js'), 'utf8');
const style = fs.readFileSync(path.join(__dirname, '../desktop/ui/style.css'), 'utf8');

test('every To-Do card opens its own detail after the list is rendered', async () => {
  const nodes = new Map(), opened = [];
  const node = props => ({ ...props, replaceChildren(...children) { this.children = children; }, setAttribute() {} });
  const $ = selector => {
    if (!nodes.has(selector)) nodes.set(selector, node({ value: '' }));
    return nodes.get(selector);
  };
  $('#todo-provider').value = 'azure'; $('#todo-source').value = 'ado'; $('#todo-limit').value = '10';
  const items = [7, 8, 9].map(id => ({ id, title: `Item ${id}`, dates: [] }));
  const context = vm.createContext({ $, window: { scrollY: 0 },
    el: (tag, props, ...children) => node({ tag, ...props, children }), badge: () => null,
    todoOriginal: () => null, todoDate: () => null, t: x => x, tx: x => x, tn: () => '',
    closeTodoDetail() {}, todoSources: () => [{}],
    invoke: async () => ({ items, states: [], assignment_fields: [] }),
    loadTodoDetail: () => opened.push(vm.runInContext('todoSelection.item.id', context)),
  });
  vm.runInContext('let todoRequest = 0, todoSelection, todoListScroll, todoReturnFocus;\n' +
    source.slice(source.indexOf('function todoStateStyle('), source.indexOf('\nfunction closeTodoDetail(')) +
    source.slice(source.indexOf('async function loadTodo()'), source.indexOf('// ---------------------------------------------------------------- settings sections')), context);
  await vm.runInContext('loadTodo()', context);
  for (const card of $('#todo-list').children) await card.children[0].children[0].children[0].onclick();
  assert.deepEqual(opened, [7, 8, 9]);
});

test('Bug Desarrollo opens task detail and workflow states get distinct colors', () => {
  const nodes = new Map(), opened = [];
  const node = props => ({ ...props, children: props?.children ?? [] });
  const $ = selector => {
    if (!nodes.has(selector)) nodes.set(selector, node({ value: '' }));
    return nodes.get(selector);
  };
  const context = vm.createContext({ $, window: { scrollY: 0 },
    el: (tag, props, ...children) => node({ tag, ...props, children }),
    badge: (text, tone) => node({ text, tone }), todoOriginal: () => null, todoDate: () => null,
    t: x => x, tx: x => x, loadTodoDetail: () => opened.push(vm.runInContext('todoSelection.task', context)),
  });
  vm.runInContext('let todoSelection, todoListScroll, todoReturnFocus;\n' +
    source.slice(source.indexOf('function todoStateStyle('), source.indexOf('\nfunction closeTodoDetail(')), context);
  const card = vm.runInContext(`todoItem(${JSON.stringify({ id: 27176, title: 'Bug Desarrollo', kind: 'Bug Desarrollo', state: 'Done', project: 'Payments', created_at: null, dates: [] })})`, context);
  assert.equal(card.class, 'todo-item done');
  assert.equal(card.children[1].children[0].tone, 'ok');
  const stateStyles = JSON.parse(vm.runInContext(`JSON.stringify(['Active', 'Resolved', 'New'].map(state => {
    const item = ${JSON.stringify({ id: 1, title: 'Item', kind: 'HU', project: 'Payments', created_at: null, dates: [] })};
    item.state = state;
    const card = todoItem(item);
    return [card.class, card.children[1].children[0].tone];
  }))`, context));
  assert.deepEqual(stateStyles, [
    ['todo-item in-progress', 'active'],
    ['todo-item review', 'warn'],
    ['todo-item unstarted', 'muted'],
  ]);
  card.children[0].children[0].children[0].onclick();
  assert.deepEqual(opened, [true]);
  assert.match(style, /\.todo-item\.done\s*\{\s*background:\s*var\(--ok-soft\);\s*border-color:\s*var\(--ok\);\s*\}/);
  assert.match(style, /\.todo-item\.in-progress\s*\{\s*background:\s*var\(--active-soft\);\s*border-color:\s*var\(--active\);\s*\}/);
  assert.match(style, /\.todo-item\.review\s*\{\s*background:\s*var\(--warn-soft\);\s*border-color:\s*var\(--warn\);\s*\}/);
  assert.match(style, /\.todo-item\.unstarted\s*\{\s*background:\s*var\(--muted-soft\);\s*border-color:\s*var\(--muted-fill\);\s*\}/);
});

test('To-Do work item actions include a copy button that copies the original URL', async () => {
  const copied = [], notices = [];
  const context = vm.createContext({
    window: { ptaTransport: { browser: true } },
    el: (tag, props, ...children) => ({ tag, ...props, children }),
    icon: name => ({ icon: name }), t: key => key,
    copy: async url => { copied.push(url); return true; },
    busy: async (_button, task) => task(),
    done: message => notices.push(['ok', message]), fail: message => notices.push(['error', message]),
  });
  vm.runInContext(source.slice(source.indexOf('function todoOriginal('), source.indexOf('\nlet todoDetailRequest')), context);
  const actions = vm.runInContext("todoOriginal({ id: 27176, url: 'https://dev.azure.com/example/_workitems/edit/27176' })", context);
  assert.equal(actions.class, 'todo-actions');
  assert.equal(actions.children[0].children[0].icon, 'link');
  const copyButton = actions.children[1];
  assert.equal(copyButton['aria-label'], 'Copy work item link');
  assert.equal(copyButton.children[0].icon, 'copy');
  await copyButton.onclick();
  assert.deepEqual(copied, ['https://dev.azure.com/example/_workitems/edit/27176']);
  assert.deepEqual(notices, [['ok', 'Link copied: {url}']]);
  assert.match(style, /\.todo-actions\s*\{[^}]*display:\s*flex/);
  assert.match(i18n, /'Copy work item link': 'Copiar enlace del work item'/);
});

test('days below or above the hours target and empty days can all be opened', async () => {
  const opened = [];
  const context = vm.createContext({
    localStorage: { getItem: () => 'es' }, navigator: { language: 'es-CL' }, Intl,
    el: (tag, props, ...children) => ({ tag, ...props, children }), badge: (text, tone) => ({ text, tone }), icon: () => ({}),
    dayText: day => day, hoursText: hours => `${hours} h`, window: { scrollY: 20 },
    openActivityDay: async day => opened.push(day), fail: assert.fail,
  });
  vm.runInContext(i18n, context);
  vm.runInContext('let activityDaysScroll, activityDayFocus;\n' + source.slice(source.indexOf('function activityDayCard('), source.indexOf('\nfunction showActivityView(')), context);
  for (const [tasks, hours] of [[0, 0], [2, 3], [10, 30]]) {
    context.day = { day: '2026-10-14', tasks, hours, target_hours: 9 };
    const card = vm.runInContext('activityDayCard(day)', context);
    assert.equal(JSON.stringify(card).includes('Por debajo de la meta diaria'), hours < 9);
    await card.onclick();
  }
  assert.deepEqual(opened, ['2026-10-14', '2026-10-14', '2026-10-14']);
});

test('browser activity navigation needs no native window operation', async () => {
  const calls = [], tabs = [];
  const context = vm.createContext({ window: { ptaTransport: { browser: true, invoke: async (...args) => calls.push(args) } }, showTab: (...args) => tabs.push(args) });
  vm.runInContext(source.slice(0, source.indexOf('\nconst $ =')), context);
  const result = await vm.runInContext("invoke('activity_open')", context);
  assert.equal(result.visible, true);
  assert.deepEqual(calls, []);
  assert.deepEqual(tabs, [['registration']]);
});
