// Run with Bun, already used by CI: exercise the actual workflow's routing and merge gate.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const workflow = Bun.YAML.parse(readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8'));
const { jobs } = workflow;

// These workflow conditions use only property access, equality and boolean operators.
function enabled(condition, event, outputs) {
  if (condition === undefined) return true;
  return Function('github', 'needs', `return (${condition});`)(
    { event_name: event }, { changes: { outputs } },
  );
}

function filter(event, files, { published = true, base = 'a'.repeat(40) } = {}) {
  const temp = mkdtempSync(join(tmpdir(), 'pta-ci-policy-'));
  try {
    // Only external inputs are synthetic; execute the filter from the real YAML unchanged.
    const script = `
      curl() { printf '%s' "$CI_TEST_HTTP"; }
      git() { if [ "$1" = diff ]; then printf '%s\\n' "$CI_TEST_FILES"; fi; }
      ${jobs.changes.steps.find(step => step.id === 'filter').run}
    `;
    const result = spawnSync('bash', ['-e', '-c', script], {
      cwd: new URL('..', import.meta.url), encoding: 'utf8',
      env: { ...process.env, EVENT: event, REF: 'refs/heads/main', BASE: base,
        GITHUB_SHA: 'b'.repeat(40), GITHUB_OUTPUT: join(temp, 'output'),
        GITHUB_STEP_SUMMARY: join(temp, 'summary'), CI_TEST_FILES: files.join('\n'),
        CI_TEST_HTTP: published ? '200' : '404' },
    });
    assert.equal(result.status, 0, result.stderr);
    return Object.fromEntries(readFileSync(join(temp, 'output'), 'utf8').trim().split('\n')
      .map(line => line.split('=')));
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

for (const event of ['pull_request', 'push']) {
  for (const [files, expected] of [
    [['src/main.rs'], { rust: 'true', ui: 'false' }],
    [['desktop/ui/app.js'], { rust: 'true', ui: 'true' }],
    [['.github/workflows/ci.yml'], { rust: 'true', ui: 'true' }],
    [['scripts/test-ci-workflow.mjs'], { rust: 'false', ui: 'true' }],
    [['docs/architecture.md', 'README.md'], { rust: 'false', ui: 'false' }],
  ]) {
    assert.deepEqual(filter(event, files), expected, `${event}: ${files}`);
  }
}
assert.deepEqual(filter('push', [], { published: false }), { rust: 'true', ui: 'true' });
assert.deepEqual(filter('pull_request', [], { base: '' }), { rust: 'true', ui: 'true' });

for (const event of ['pull_request', 'push', 'schedule']) {
  const outputs = { rust: 'true', ui: 'true' };
  assert(enabled(jobs.linux.if, event, outputs), `${event}: final Linux verification`);
  assert(enabled(jobs.security.if, event, outputs), `${event}: security must run`);
  for (const id of ['ui', 'macos', 'windows']) {
    assert.equal(enabled(jobs[id].if, event, outputs), event !== 'push', `${event}: ${id}`);
    assert.equal(enabled(jobs[id].if, event, { rust: 'false', ui: 'false' }), false);
  }
  const commands = jobs.linux.steps.filter(step => step.run && enabled(step.if, event, outputs))
    .map(step => step.run);
  for (const command of ['cargo test --locked', 'cargo test --locked --no-default-features', 'cargo package --locked']) {
    assert(commands.includes(command), `${event}: ${command} must verify the final commit`);
  }
  assert.equal(commands.includes('cargo fmt --all --check'), event !== 'push');
  assert.equal(commands.some(command => command.startsWith('cargo clippy')), event !== 'push');
}

assert.deepEqual(filter('schedule', []), { rust: 'true', ui: 'true' });
assert.equal(jobs.changes.if, undefined, 'Weekly runs must reach the full matrix');

assert.equal(jobs.result.if, 'always()');
assert.equal(jobs.result.name, 'CI result');
for (const failed of [null, 'linux', 'security', 'changes', 'ui', 'macos', 'windows']) {
  for (const outcome of ['failure', 'cancelled']) {
    const needs = Object.fromEntries(jobs.result.needs.map(id => [id, {
      result: id === failed ? outcome : ['ui', 'macos', 'windows'].includes(id) ? 'skipped' : 'success',
    }]));
    const result = spawnSync('bash', ['-e', '-c', jobs.result.steps[0].run], {
      encoding: 'utf8', env: { ...process.env, NEEDS: JSON.stringify(needs), GITHUB_STEP_SUMMARY: '/dev/null' },
    });
    assert.equal(result.status, failed && failed !== 'windows' ? 1 : 0, `${failed}: ${result.stderr}`);
  }
}
console.log('CI policy passed: PR/full weekly matrix, main tests/package, path filters and required-job failures.');
