// tools/no-cyrillic.sh (cyrillic part of tools/check.sh, step 2.1 of the factory plan): catches Cyrillic in the content and
// names of files, lets exactly hub-web/src/lib/i18n/ru.ts through and does not come out green when grep lacks -P (busybox
// in alpine/k8s: without the probe the grep error was swallowed and the script printed "clean"). A sample repo in a temporary directory.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const script = join(dirname(fileURLToPath(import.meta.url)), 'no-cyrillic.sh');
// Cyrillic samples come from the Russian UI dictionary, the one file allowed to hold Cyrillic, so that this file
// carries none, not even as escapes.
const [HELLO, NAME] = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../hub-web/src/lib/i18n/ru.ts'), 'utf8')
  .match(/\p{Script=Cyrillic}+/gu);

function sample(files, { git = true } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'no-cyrillic-'));
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  }
  if (git) {
    const run = (...args) => assert.equal(spawnSync('git', ['-C', root, ...args]).status, 0);
    run('init', '-q');
    run('add', '-A');
  }
  return root;
}

function check(root, dirs, env = {}) {
  const r = spawnSync('sh', [script, ...dirs], { encoding: 'utf8', env: { ...process.env, CHECK_SH_ROOT: root, ...env } });
  return { status: r.status, out: r.stdout + r.stderr };
}

const base = {
  'hub-web/src/lib/i18n/ru.ts': `export const hello = "${HELLO}";\n`,
  'hub-web/src/routes/clean.ts': 'export const hello = "Hello";\n',
};

test('content and file names with Cyrillic fail, ru.ts passes', (t) => {
  for (const git of [true, false]) {
    const root = sample({ ...base, 'hub-web/src/routes/page.svelte': `<p>${HELLO}</p>\n`, [`hub-web/src/routes/${NAME}.ts`]: 'x\n' }, { git });
    t.after(() => rmSync(root, { recursive: true, force: true }));
    const r = check(root, ['hub-web']);
    assert.equal(r.status, 1, r.out);
    assert.match(r.out, /hub-web\/src\/routes\/page\.svelte/);
    assert.match(r.out, new RegExp(`hub-web/src/routes/${NAME}\\.ts \\(name\\)`));
    assert.doesNotMatch(r.out, /i18n\/ru\.ts/);
  }
});

test('a clean tree passes and only exactly ru.ts is allowed', (t) => {
  const root = sample({ ...base, 'tools/ru.ts': `const a = "${HELLO}";\n` });
  t.after(() => rmSync(root, { recursive: true, force: true }));
  assert.equal(check(root, ['hub-web']).status, 0);
  const r = check(root, ['tools']);
  assert.equal(r.status, 1, r.out);
  assert.match(r.out, /tools\/ru\.ts/);
});

test('without .git installed dependencies are skipped like git ls-files does', (t) => {
  const root = sample({ ...base, 'apphost/apphost.mts': 'x\n', 'apphost/node_modules/pkg/README.md': `${HELLO}\n` }, { git: false });
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const r = check(root, ['apphost']);
  assert.equal(r.status, 0, r.out);
});

test('a missing directory is an error, not an empty pass', (t) => {
  const root = sample(base);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  assert.equal(check(root, ['hub-web/nope']).status, 2);
});

test('grep without -P fails instead of reporting a clean tree', (t) => {
  const root = sample({ ...base, 'hub-web/src/routes/page.svelte': `<p>${HELLO}</p>\n` });
  // grep like busybox: no -P flag, exit code 2.
  const bin = join(root, '.bin');
  mkdirSync(bin);
  writeFileSync(join(bin, 'grep'), '#!/bin/sh\nfor a; do case "$a" in -*P*) echo "grep: unrecognized option: P" >&2; exit 2;; esac; done\nPATH=/usr/bin:/bin exec grep "$@"\n');
  chmodSync(join(bin, 'grep'), 0o755);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const r = check(root, ['hub-web'], { PATH: bin + ':' + process.env.PATH });
  assert.equal(r.status, 2, r.out);
  assert.match(r.out, /grep -P/);
});
