// Run with Bun, already used by CI: exercise the actual workflow's routing and merge gate.
import assert from 'node:assert/strict';
import { mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const source = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8');
const workflow = Bun.YAML.parse(source);
const { jobs } = workflow;

// These workflow conditions use only property access, equality and boolean operators.
function enabled(condition, event, outputs) {
  if (condition === undefined) return true;
  return Function('github', 'needs', `return (${condition});`)(
    { event_name: event }, { changes: { outputs } },
  );
}

function filter(event, files, { published = true, base = 'a'.repeat(40), summary = false } = {}) {
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
    if (summary) return readFileSync(join(temp, 'summary'), 'utf8');
    return Object.fromEntries(readFileSync(join(temp, 'output'), 'utf8').trim().split('\n')
      .map(line => line.split('=')));
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

// `platform` is the pull-request verdict; a push to main that is not a release candidate never
// runs macOS or Windows, because its pull request already did when it mattered.
const neutral = { rust: 'false', ui: 'false', cloud: 'false', platform: 'false' };
for (const [files, expected] of [
  [['src/pipeline.rs'], { rust: 'true', platform: 'false' }],
  [['src/app/web.rs', 'tests/integration.rs'], { rust: 'true', platform: 'false' }],
  [['desktop/ui/app.js'], { rust: 'true', ui: 'true', platform: 'false' }],
  [['.github/workflows/ci.yml'], { rust: 'true', ui: 'true', cloud: 'true', platform: 'true' }],
  [['.github/workflows/release.yml'], { rust: 'true', ui: 'true', cloud: 'true', platform: 'true' }],
  [['scripts/test-ci-workflow.mjs'], { ui: 'true', platform: 'false' }],
  [['scripts/test-windows-startup.py'], { ui: 'true', platform: 'true' }],
  [['docs/architecture.md', 'README.md'], {}],
  [['cloud/src/index.ts'], { cloud: 'true' }],
  [['Cargo.toml'], { rust: 'true', platform: 'true' }],
  [['Cargo.lock'], { rust: 'true', platform: 'true' }],
  [['rust-toolchain.toml'], { rust: 'true', platform: 'true' }],
  [['build.rs'], { rust: 'true', platform: 'true' }],
  [['src/security/mod.rs'], { rust: 'true', platform: 'true' }],
  [['src/maintenance/apply.rs'], { rust: 'true', platform: 'true' }],
  [['src/app/maintenance/apply.rs'], { rust: 'true', platform: 'true' }],
  [['src/llm/implementation/codex.rs'], { rust: 'true', platform: 'true' }],
  [['src/app/gui.rs'], { rust: 'true', platform: 'true' }],
  [['desktop/tauri.conf.json'], { rust: 'true', platform: 'true' }],
  [['src/pipeline.rs', 'src/consumers.rs'], { rust: 'true', platform: 'true' }],
]) {
  assert.deepEqual(filter('pull_request', files), { ...neutral, ...expected }, `pull_request: ${files}`);
  assert.deepEqual(filter('push', files), { ...neutral, ...expected, platform: 'false' }, `push: ${files}`);
}
assert.match(filter('pull_request', ['src/pipeline.rs'], { summary: true }), /macOS and Windows: false\. .*none is platform-sensitive/);
assert.match(filter('pull_request', ['src/pipeline.rs', 'Cargo.lock'], { summary: true }),
  /macOS and Windows: true\. Reason: changed paths; platform-sensitive: Cargo\.lock\.\n$/);

// The list is the only place that names platform-sensitive paths, and it must cover every source
// file whose code differs on Windows or macOS.
function* rustFiles(dir) {
  for (const entry of readdirSync(new URL(`../${dir}`, import.meta.url), { withFileTypes: true })) {
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory()) yield* rustFiles(path);
    else if (path.endsWith('.rs')) yield path;
  }
}
const nativeOnly = /cfg!?\((?:all\(|any\()?(?:not\()?(?:windows|target_os = "(?:windows|macos)")|cfg!?\(not\(unix\)\)/;
const native = [...rustFiles('src'), ...rustFiles('tests'), 'build.rs']
  .filter(path => nativeOnly.test(readFileSync(new URL(`../${path}`, import.meta.url), 'utf8')));
assert(native.length >= 10, `expected platform-specific sources, found ${native}`);
for (const path of native) {
  assert.equal(filter('pull_request', [path]).platform, 'true', `${path} has Windows- or macOS-only code`);
}
assert.equal(source.match(/^ *platform=true$/gm).length, 1, 'one list of platform-sensitive paths');

const all = { rust: 'true', ui: 'true', cloud: 'true', platform: 'true' };
// A release candidate on main runs every platform before release.yml publishes it.
assert.deepEqual(filter('push', [], { published: false }), all);
assert.deepEqual(filter('push', ['docs/delivery.md'], { published: false }), all);
assert.deepEqual(filter('pull_request', [], { base: '' }), all);
assert.deepEqual(filter('push', [], { base: '' }), { ...all, platform: 'false' });

for (const event of ['pull_request', 'push', 'schedule']) {
  const outputs = { rust: 'true', ui: 'true', cloud: 'true', platform: 'true' };
  assert(enabled(jobs.linux.if, event, outputs), `${event}: final Linux verification`);
  assert(enabled(jobs.security.if, event, outputs), `${event}: security must run`);
  for (const id of ['ui', 'cloud']) {
    assert.equal(enabled(jobs[id].if, event, outputs), event !== 'push', `${event}: ${id}`);
    assert.equal(enabled(jobs[id].if, event, { ...outputs, ui: 'false', cloud: 'false' }), false);
  }
  // macOS and Windows follow only the platform verdict, on every event.
  for (const id of ['macos', 'windows']) {
    assert.equal(enabled(jobs[id].if, event, outputs), true, `${event}: ${id}`);
    assert.equal(enabled(jobs[id].if, event, { ...outputs, platform: 'false' }), false, `${event}: ${id}`);
  }
  // The Worker job holds no secrets and runs on a standard runner.
  assert.equal(jobs.cloud['runs-on'], 'ubuntu-latest');
  assert(!JSON.stringify(jobs.cloud).includes('secrets.'), 'the Worker job uses no secrets');
  const commands = jobs.linux.steps.filter(step => step.run && enabled(step.if, event, outputs))
    .map(step => step.run);
  for (const command of ['cargo test --locked', 'cargo test --locked --no-default-features', 'cargo package --locked']) {
    assert(commands.includes(command), `${event}: ${command} must verify the final commit`);
  }
  assert.equal(commands.includes('cargo fmt --all --check'), event !== 'push');
  assert.equal(commands.some(command => command.startsWith('cargo clippy')), event !== 'push');
}

assert.deepEqual(filter('schedule', []), all);
assert.deepEqual(filter('schedule', ['docs/delivery.md']), all);
assert.equal(jobs.macos['continue-on-error'], undefined, 'macOS is required whenever it runs');
assert.equal(jobs.windows['continue-on-error'], true, 'Windows stays informative');
assert.equal(jobs.windows.name, 'Windows (informative)');
assert.equal(jobs.changes.if, undefined, 'Weekly runs must reach the full matrix');

assert.equal(jobs.result.if, 'always()');
assert.equal(jobs.result.name, 'CI result');
for (const failed of [null, 'linux', 'security', 'changes', 'ui', 'cloud', 'macos', 'windows']) {
  for (const outcome of ['failure', 'cancelled']) {
    const needs = Object.fromEntries(jobs.result.needs.map(id => [id, {
      result: id === failed ? outcome : ['ui', 'cloud', 'macos', 'windows'].includes(id) ? 'skipped' : 'success',
    }]));
    const result = spawnSync('bash', ['-e', '-c', jobs.result.steps[0].run], {
      encoding: 'utf8', env: { ...process.env, NEEDS: JSON.stringify(needs), GITHUB_STEP_SUMMARY: '/dev/null' },
    });
    assert.equal(result.status, failed && failed !== 'windows' ? 1 : 0, `${failed}: ${result.stderr}`);
  }
}
// The release waits for this workflow's run on the same commit and requires its Linux job.
const release = readFileSync(new URL('../.github/workflows/release.yml', import.meta.url), 'utf8');
assert(release.includes('actions/workflows/ci.yml/runs?head_sha=$GITHUB_SHA&event=push&branch=main'));
assert(release.includes('select(.name == "Linux")'));
assert.equal(jobs.linux.name, 'Linux');
assert.deepEqual(workflow.on.push.branches, ['main']);
assert.equal(workflow.concurrency['cancel-in-progress'], "${{ github.event_name == 'pull_request' }}");

// Playwright's Chromium comes from a cache keyed by the pinned Playwright version and downloads
// only on a miss; only main saves it.
const ui = jobs.ui.steps;
assert.match(String(jobs.ui.env.PLAYWRIGHT_CORE_VERSION), /^\d+\.\d+\.\d+$/);
assert(ui.some(step => step.run?.includes('"playwright-core@$PLAYWRIGHT_CORE_VERSION"')));
const restore = ui.find(step => step.uses?.startsWith('actions/cache/restore@'));
assert.equal(restore.with.key, 'playwright-chromium-${{ runner.os }}-${{ env.PLAYWRIGHT_CORE_VERSION }}');
assert.equal(restore.with.path, '~/.cache/ms-playwright');
const install = ui.find(step => step.run?.includes('install --with-deps chromium'));
assert.equal(install.env.CACHE_HIT, `\${{ steps.${restore.id}.outputs.cache-hit }}`);
for (const [hit, downloads] of [['true', false], ['', true], ['false', true]]) {
  const temp = mkdtempSync(join(tmpdir(), 'pta-ci-chromium-'));
  try {
    const bin = join(temp, 'pta-ui-preview/node_modules/.bin');
    const script = `mkdir -p "${bin}"
      printf '#!/bin/sh\\necho "$*" >> "$RUNNER_TEMP/calls"\\n' > "${bin}/playwright-core"
      chmod +x "${bin}/playwright-core"
      ${install.run}`;
    const result = spawnSync('bash', ['-e', '-c', script], {
      encoding: 'utf8', env: { ...process.env, RUNNER_TEMP: temp, CACHE_HIT: hit } });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(readFileSync(join(temp, 'calls'), 'utf8'),
      downloads ? 'install --with-deps chromium\n' : 'install-deps chromium\n', `cache hit '${hit}'`);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}
const save = ui.find(step => step.uses?.startsWith('actions/cache/save@'));
assert.equal(save.if, `github.ref == 'refs/heads/main' && steps.${restore.id}.outputs.cache-hit != 'true'`);
assert.equal(save.with.key, `\${{ steps.${restore.id}.outputs.cache-primary-key }}`);

// Standard GitHub-hosted runners, no secrets, actions pinned by commit, permissions per job.
assert.deepEqual(workflow.permissions, { contents: 'read' });
assert(!source.includes('secrets.'), 'ci.yml uses no secrets');
for (const [id, job] of Object.entries(jobs)) {
  assert(['ubuntu-latest', 'macos-latest', 'windows-latest'].includes(job['runs-on']), `${id}: standard runner`);
  for (const step of job.steps) {
    if (step.uses) assert.match(step.uses, /^[\w.-]+\/[\w./-]+@[0-9a-f]{40}$/, `${id}: ${step.uses} pinned by commit`);
  }
}
for (const line of source.split('\n').filter(line => /^\s*(- )?uses: /.test(line))) {
  assert.match(line, /@[0-9a-f]{40} # v\d+(\.\d+)*$/, `version comment: ${line.trim()}`);
}

console.log('CI policy passed: Linux PR gate, platform paths, weekly and release-candidate matrix, main tests/package, Chromium cache, the Worker job and required-job failures.');
