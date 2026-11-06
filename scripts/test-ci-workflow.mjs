// Run with Bun, already used by CI: exercise the actual workflow's routing and merge gate.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
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

// `manifests`: what `git show` returns for Cargo.toml and Cargo.lock at the base (`old`) and the
// head (`new`); without it, it returns nothing, as for a file the base does not have.
function filter(event, files, { published = true, base = 'a'.repeat(40), summary = false, manifests = {} } = {}) {
  const temp = mkdtempSync(join(tmpdir(), 'pta-ci-policy-'));
  try {
    for (const [side, contents] of Object.entries(manifests)) {
      mkdirSync(join(temp, side));
      for (const [name, text] of Object.entries(contents)) writeFileSync(join(temp, side, name), text);
    }
    // Only external inputs are synthetic; execute the filter from the real YAML unchanged.
    const script = `
      curl() { printf '%s' "$CI_TEST_HTTP"; }
      git() {
        case "$1" in
          diff) printf '%s\\n' "$CI_TEST_FILES" ;;
          show) case "$2" in "$BASE":*) side=old ;; *) side=new ;; esac
            cat "$CI_TEST_SHOW/$side/\${2#*:}" 2> /dev/null ;;
        esac
      }
      ${jobs.changes.steps.find(step => step.id === 'filter').run}
    `;
    const result = spawnSync('bash', ['-e', '-c', script], {
      cwd: new URL('..', import.meta.url), encoding: 'utf8',
      env: { ...process.env, EVENT: event, REF: 'refs/heads/main', BASE: base,
        GITHUB_SHA: 'b'.repeat(40), GITHUB_OUTPUT: join(temp, 'output'),
        GITHUB_STEP_SUMMARY: join(temp, 'summary'), CI_TEST_FILES: files.join('\n'), CI_TEST_SHOW: temp,
        CI_TEST_HTTP: published ? '200' : '404' },
    });
    assert.equal(result.status, 0, result.stderr);
    if (summary) return readFileSync(join(temp, 'summary'), 'utf8');
    const outputs = Object.fromEntries(readFileSync(join(temp, 'output'), 'utf8').trim().split('\n')
      .map(line => line.split('=')));
    // Off a release candidate, macOS and Windows share one verdict, so the tables below name it once.
    if (outputs.macos === outputs.windows) {
      const { macos, windows, ...rest } = outputs;
      return { ...rest, platform: macos };
    }
    return outputs;
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

// `platform` is the pull-request verdict for both macOS and Windows; a push to main that is not a release candidate never
// runs macOS or Windows, because its pull request already did when it mattered.
const neutral = { rust: 'false', gui: 'false', ui: 'false', cloud: 'false', platform: 'false' };
for (const [files, expected] of [
  [['src/pipeline.rs'], { rust: 'true', platform: 'false' }],
  [['src/app/web.rs', 'tests/integration.rs'], { rust: 'true', platform: 'false' }],
  // A GUI-only change skips clippy and the Rust suite but runs the Rust tests that read GUI files.
  [['desktop/ui/app.js'], { gui: 'true', ui: 'true' }],
  [['desktop/ui/app.js', 'desktop/ui/style.css', 'docs/ui-design-conventions.md'], { gui: 'true', ui: 'true' }],
  [['desktop/ui/app.js', 'scripts/ui-preview/server.mjs'], { gui: 'true', ui: 'true' }],
  // With any other Rust change the full suite runs, which includes those tests.
  [['desktop/ui/app.js', 'src/app/web.rs'], { rust: 'true', ui: 'true' }],
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
  [['src/app/headless.rs'], { rust: 'true', platform: 'true' }],
  [['src/app/page.rs'], { rust: 'true', platform: 'true' }],
  [['src/app/service.rs'], { rust: 'true', platform: 'true' }],
  [['tests/service.rs'], { rust: 'true', platform: 'true' }],
  // The GUI's files and icons are the same on every platform.
  [['desktop/ui/favicon.png'], { gui: 'true', ui: 'true' }],
  [['src/pipeline.rs', 'src/consumers.rs'], { rust: 'true', platform: 'true' }],
]) {
  assert.deepEqual(filter('pull_request', files), { ...neutral, ...expected }, `pull_request: ${files}`);
  assert.deepEqual(filter('push', files), { ...neutral, ...expected, platform: 'false' }, `push: ${files}`);
}
assert.match(filter('pull_request', ['src/pipeline.rs'], { summary: true }), /macOS: false\. Windows: false\. .*none is platform-sensitive/);
assert.match(filter('pull_request', ['desktop/ui/app.js'], { summary: true }), /Rust checks: false\. GUI asset tests only: true\./);

// The version bump a GUI change is delivered with is neither a Rust nor a platform change; any
// other change to the manifest or the lock is both.
const toml = readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8');
const lock = readFileSync(new URL('../Cargo.lock', import.meta.url), 'utf8');
const version = Bun.TOML.parse(toml).package.version;
const ours = `name = "personal-teams-assistant"\nversion = "${version}"`;
assert(lock.includes(ours), 'Cargo.lock names this package and its version');
const bumped = {
  'Cargo.toml': toml.replace(`\nversion = "${version}"`, '\nversion = "999.0.0"'),
  'Cargo.lock': lock.replace(ours, 'name = "personal-teams-assistant"\nversion = "999.0.0"'),
};
assert.notEqual(bumped['Cargo.toml'], toml);
const old = { 'Cargo.toml': toml, 'Cargo.lock': lock };
const delivered = ['desktop/ui/app.js', 'Cargo.toml', 'Cargo.lock'];
for (const event of ['pull_request', 'push']) {
  assert.deepEqual(filter(event, delivered, { manifests: { old, new: bumped } }), { ...neutral, gui: 'true', ui: 'true' }, `${event}: GUI change and version bump`);
  assert.deepEqual(filter(event, ['Cargo.toml', 'Cargo.lock'], { manifests: { old, new: bumped } }), { ...neutral, gui: 'true' }, `${event}: version bump`);
  const expected = { ...neutral, rust: 'true', ui: 'true', platform: event === 'push' ? 'false' : 'true' };
  for (const changed of [
    { ...bumped, 'Cargo.toml': bumped['Cargo.toml'].replace('[dependencies]', '[dependencies]\nsynthetic-crate = "1"') },
    { ...bumped, 'Cargo.lock': bumped['Cargo.lock'].replace(/(name = "serde"\nversion = ")[^"]+/, '$1999.0.0') },
    { 'Cargo.toml': '' , 'Cargo.lock': '' },
  ]) {
    assert.notDeepEqual(changed, bumped);
    assert.deepEqual(filter(event, delivered, { manifests: { old, new: changed } }), expected, `${event}: manifest change`);
  }
}
assert.match(filter('pull_request', ['src/pipeline.rs', 'Cargo.lock'], { summary: true }),
  /macOS: true\. Windows: true\. Reason: changed paths; platform-sensitive: Cargo\.lock\.\n$/);

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

const all = { rust: 'true', gui: 'false', ui: 'true', cloud: 'true', platform: 'true' };
// A release candidate on main runs Linux and macOS, required, before release.yml publishes it, but
// not the informative Windows job, which would delay every publication.
const candidate = { rust: 'true', gui: 'false', ui: 'true', cloud: 'true', macos: 'true', windows: 'false' };
assert.deepEqual(filter('push', [], { published: false }), candidate);
assert.deepEqual(filter('push', ['docs/delivery.md'], { published: false }), candidate);
assert.deepEqual(filter('push', ['Cargo.toml', 'Cargo.lock', 'src/security/mod.rs'], { published: false }), candidate);
assert.match(filter('push', [], { published: false, summary: true }), /macOS: true\. Windows: false\. .*will be released/);
assert.deepEqual(filter('pull_request', [], { base: '' }), all);
assert.deepEqual(filter('push', [], { base: '' }), { ...all, platform: 'false' });

for (const event of ['pull_request', 'push', 'schedule']) {
  const outputs = { rust: 'true', ui: 'true', cloud: 'true', macos: 'true', windows: 'true' };
  assert(enabled(jobs.linux.if, event, outputs), `${event}: final Linux verification`);
  assert(enabled(jobs.security.if, event, outputs), `${event}: security must run`);
  for (const id of ['ui', 'cloud']) {
    assert.equal(enabled(jobs[id].if, event, outputs), event !== 'push', `${event}: ${id}`);
    assert.equal(enabled(jobs[id].if, event, { ...outputs, ui: 'false', cloud: 'false' }), false);
  }
  // macOS and Windows each follow only their own verdict, on every event.
  for (const id of ['macos', 'windows']) {
    assert.equal(enabled(jobs[id].if, event, outputs), true, `${event}: ${id}`);
    assert.equal(enabled(jobs[id].if, event, { ...outputs, [id]: 'false' }), false, `${event}: ${id}`);
  }
  const releaseCandidate = { ...outputs, windows: 'false' };
  assert.equal(enabled(jobs.macos.if, event, releaseCandidate), true);
  assert.equal(enabled(jobs.windows.if, event, releaseCandidate), false);
  // The Worker job holds no secrets and runs on a standard runner.
  assert.equal(jobs.cloud['runs-on'], 'ubuntu-latest');
  assert(!JSON.stringify(jobs.cloud).includes('secrets.'), 'the Worker job uses no secrets');
  const commands = jobs.linux.steps.filter(step => step.run && enabled(step.if, event, outputs))
    .map(step => step.run);
  for (const command of ['cargo test --locked', 'cargo package --locked']) {
    assert(commands.includes(command), `${event}: ${command} must verify the final commit`);
  }
  assert.equal(commands.includes('cargo fmt --all --check'), event !== 'push');
  assert.equal(commands.some(command => command.startsWith('cargo clippy')), event !== 'push');
  // One build: each check runs once, without a feature matrix or the window's system libraries.
  assert.equal(commands.filter(command => command.startsWith('cargo clippy')).length, event === 'push' ? 0 : 1);
  assert.equal(commands.filter(command => command.startsWith('cargo test')).length, 1);
}

assert.deepEqual(filter('schedule', []), all);
assert.deepEqual(filter('schedule', ['docs/delivery.md']), all);
assert.equal(jobs.macos['continue-on-error'], undefined, 'macOS is required whenever it runs');
assert.equal(jobs.windows['continue-on-error'], true, 'Windows stays informative');
assert.equal(jobs.windows.name, 'Windows (informative)');
assert.equal(jobs.changes.if, undefined, 'Weekly runs must reach the full matrix');

assert.equal(jobs.result.if, 'always()');
assert.equal(jobs.result.name, 'CI result');
for (const failed of [null, 'linux', 'gui', 'security', 'changes', 'ui', 'cloud', 'macos', 'windows']) {
  for (const outcome of ['failure', 'cancelled']) {
    const needs = Object.fromEntries(jobs.result.needs.map(id => [id, {
      result: id === failed ? outcome : ['gui', 'ui', 'cloud', 'macos', 'windows'].includes(id) ? 'skipped' : 'success',
    }]));
    const result = spawnSync('bash', ['-e', '-c', jobs.result.steps[0].run], {
      encoding: 'utf8', env: { ...process.env, NEEDS: JSON.stringify(needs), GITHUB_STEP_SUMMARY: '/dev/null' },
    });
    assert.equal(result.status, failed && failed !== 'windows' ? 1 : 0, `${failed}: ${result.stderr}`);
  }
}
// The narrowed job runs exactly the Rust tests named gui_assets, only when no full Rust run does,
// and they are the only Rust code that reads desktop/ui.
for (const event of ['pull_request', 'push']) {
  assert.equal(enabled(jobs.gui.if, event, { rust: 'false', gui: 'true' }), true);
  assert.equal(enabled(jobs.gui.if, event, { rust: 'true', gui: 'true' }), false);
  assert.equal(enabled(jobs.gui.if, event, { rust: 'false', gui: 'false' }), false);
}
assert.deepEqual(jobs.gui.steps.filter(step => step.run?.startsWith('cargo')).map(step => step.run),
  ['cargo test --locked --lib gui_assets']);
assert(!JSON.stringify(jobs.gui).includes('clippy'));
const readsGui = /include_(?:str|bytes)!\([^)]*desktop\/ui|"[^"\n]*desktop\/ui/;
const guiReaders = [...rustFiles('src'), ...rustFiles('tests'), 'build.rs']
  .filter(path => readsGui.test(readFileSync(new URL(`../${path}`, import.meta.url), 'utf8')));
assert.deepEqual(guiReaders.sort(), ['src/app/gui_assets.rs', 'src/app/gui_assets/tests.rs'],
  'only app::gui_assets reads desktop/ui; its tests are the ones the gui job runs');
for (const listener of ['src/app/page/tests.rs', 'src/app/web/tests.rs']) {
  assert.match(readFileSync(new URL(`../${listener}`, import.meta.url), 'utf8'), /async fn gui_assets_are_served_by_/, listener);
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

// No job installs the window's system libraries, checks a third version file or builds a feature matrix.
for (const banned of ['webkit2gtk', 'libgtk', 'appindicator', 'tauri', 'Versions agree', '--no-default-features', '--all-features']) {
  assert(!source.toLowerCase().includes(banned.toLowerCase()), `ci.yml mentions ${banned}`);
}
const windows = jobs.windows.steps.filter(step => step.run).map(step => step.run).join('\n');
assert(windows.includes('python scripts/test-windows-startup.py'), 'Windows checks headless startup, quit and relaunch');

// Standard GitHub-hosted runners, no secrets, actions pinned by commit, permissions per job.
assert.deepEqual(workflow.permissions, { contents: 'read' });
assert(!source.includes('secrets.'), 'ci.yml uses no secrets');
for (const [id, job] of Object.entries(jobs)) {
  assert(['ubuntu-latest', 'macos-latest', 'windows-latest'].includes(job['runs-on'])
    || (id === 'ui' && job['runs-on'] === 'ubuntu-24.04'), `${id}: standard runner`);
  for (const step of job.steps) {
    if (step.uses) assert.match(step.uses, /^[\w.-]+\/[\w./-]+@[0-9a-f]{40}$/, `${id}: ${step.uses} pinned by commit`);
  }
}
for (const line of source.split('\n').filter(line => /^\s*(- )?uses: /.test(line))) {
  assert.match(line, /@[0-9a-f]{40} # v\d+(\.\d+)*$/, `version comment: ${line.trim()}`);
}

// Release archives: one package, one build and one archive name on all four targets, which
// [package.metadata.binstall] resolves (the overrides change only the Windows archive format).
const binaries = Bun.YAML.parse(readFileSync(new URL('../.github/workflows/binaries.yml', import.meta.url), 'utf8'));
const manifest = Bun.TOML.parse(readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8'));
const targets = binaries.jobs.build.strategy.matrix.include;
assert.deepEqual(targets.map(entry => entry.target).sort(),
  ['aarch64-apple-darwin', 'x86_64-apple-darwin', 'x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu']);
for (const entry of targets) assert.deepEqual(Object.keys(entry).sort(), ['os', 'target'], `${entry.target}: no flags or own name`);
const build = binaries.jobs.build.steps.find(step => step.name === 'Release build').run;
assert.equal(build, 'cargo build --release --locked --target "$TARGET"');
const archive = binaries.jobs.build.steps.find(step => step.name === 'Archive');
assert.equal(archive.env.NAME, manifest.package.name);
assert(archive.run.includes('dir="$NAME-$TARGET-v$version"'));
const binstall = manifest.package.metadata.binstall;
assert.equal(binstall['pkg-url'], '{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }{ archive-suffix }');
assert.equal(binstall['bin-dir'], '{ name }-{ target }-v{ version }/{ bin }{ binary-ext }');
assert.deepEqual(Object.keys(binstall.overrides), ['x86_64-pc-windows-msvc']);
assert.deepEqual(binstall.overrides['x86_64-pc-windows-msvc'], { 'pkg-fmt': 'zip' });
// The one-command installers: attached to every release with digests and provenance, run by each
// build job against its own archive, and never in test mode outside those steps.
assert(binaries.on.pull_request.paths.includes('scripts/install/**'), 'installer changes build and test the archives');
const steps = binaries.jobs.build.steps;
const unixInstaller = steps.find(step => step.name === 'Installer (macOS, Linux)');
const windowsInstaller = steps.find(step => step.name === 'Installer (Windows PowerShell)');
assert(unixInstaller.run.includes('sh scripts/install/test-install.sh dist'), 'macOS and Linux run install.sh on the archive just built');
assert(windowsInstaller.run.includes('powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/install/test-install.ps1'), 'Windows runs install.ps1 under Windows PowerShell');
assert(steps.indexOf(unixInstaller) > steps.indexOf(archive) && steps.indexOf(windowsInstaller) > steps.indexOf(archive));
const publish = binaries.jobs.publish;
const attach = publish.steps.find(step => step.name === 'Installer scripts and their digests');
assert(attach.run.includes('for file in install.sh install.ps1') && attach.run.includes('sha256sum'), 'both scripts are attached with a digest');
const attest = publish.steps.find(step => step.name === 'Build provenance attestation');
for (const file of ['dist/install.sh', 'dist/install.ps1']) assert(attest.with['subject-path'].includes(file), `${file} has provenance`);
assert(!JSON.stringify(publish).includes('PTA_INSTALL_TEST') && !JSON.stringify(binaries.jobs.lint).includes('PTA_INSTALL_TEST'), 'no release step enables the test source');
assert(binaries.jobs.lint.steps.some(step => step.run?.includes('shellcheck')) && binaries.jobs.lint.steps.some(step => step.run?.includes('Invoke-ScriptAnalyzer')));
for (const name of ['install.sh', 'install.ps1']) {
  const script = readFileSync(new URL(`../scripts/install/${name}`, import.meta.url), 'utf8');
  assert(script.includes(`'${manifest.package.repository.replace('https://github.com/', '')}'`) || script.includes(`"${manifest.package.repository.replace('https://github.com/', '')}"`), `${name} downloads from the package's own repository`);
}

// The version lives in Cargo.toml and Cargo.lock only.
assert.deepEqual(manifest.features, { default: [], gui: [] }, 'gui stays as an empty, deprecated feature');
assert(!manifest.package.include.some(path => path.includes('tauri') || path.includes('Info.plist') || path.includes('capabilities')));

console.log('CI policy passed: Linux PR gate, GUI-only changes and version bumps without clippy or the Rust suite, platform paths, weekly matrix, macOS without Windows for a release candidate, main tests/package, Chromium cache, the Worker job, required-job failures, one release archive per target and the installers.');
