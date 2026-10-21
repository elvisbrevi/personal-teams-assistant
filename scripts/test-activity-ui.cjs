const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../desktop/ui/app.js'), 'utf8');
const i18n = fs.readFileSync(path.join(__dirname, '../desktop/ui/i18n.js'), 'utf8');

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
    source.slice(source.indexOf('function todoItem('), source.indexOf('\nfunction closeTodoDetail(')) +
    source.slice(source.indexOf('async function loadTodo()'), source.indexOf('// ---------------------------------------------------------------- settings sections')), context);
  await vm.runInContext('loadTodo()', context);
  for (const card of $('#todo-list').children) await card.children[0].children[0].children[0].onclick();
  assert.deepEqual(opened, [7, 8, 9]);
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
