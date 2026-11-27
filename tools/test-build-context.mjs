// The hub image is built with services/ as the build context (services/Dockerfile, both by Aspire publish and by the build
// pipeline): a file that Rust embeds at compile time (include_str!, include_bytes!, sqlx::migrate!) must lie inside
// services/, or the image build fails while cargo in the whole tree still passes. This test resolves every such path.
import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = resolve(process.env.CHECK_SH_ROOT ?? join(dirname(fileURLToPath(import.meta.url)), '..'));
const services = join(root, 'services');

function rustFiles(dir) {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (name === 'target') return [];
    if (statSync(p).isDirectory()) return rustFiles(p);
    return name.endsWith('.rs') ? [p] : [];
  });
}

// The crate directory of a source file: the nearest parent with Cargo.toml (sqlx::migrate! paths are relative to it).
function crateDir(file) {
  let d = dirname(file);
  while (!existsSync(join(d, 'Cargo.toml'))) d = dirname(d);
  return d;
}

test('every compile-time embedded file of services/ lies inside services/', () => {
  const found = [];
  for (const file of rustFiles(services)) {
    const text = readFileSync(file, 'utf8');
    for (const m of text.matchAll(/include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)/g)) {
      found.push({ file, path: resolve(dirname(file), m[1]) });
    }
    for (const m of text.matchAll(/migrate!\(\s*"([^"]+)"\s*\)/g)) {
      found.push({ file, path: resolve(crateDir(file), m[1]) });
    }
  }
  assert.ok(found.length > 0, 'no embedded files found: the pattern no longer matches the code');
  // The migration chain is embedded only where its directory exists (cfg `chain`, services/hub/build.rs).
  const chain = join(services, 'hub/migrations');
  for (const { file, path } of found) {
    if (!existsSync(chain) && !relative(chain, path).startsWith('..')) continue;
    const where = relative(root, file);
    assert.ok(!relative(services, path).startsWith('..'), `${where} embeds ${relative(root, path)}, outside the build context services/`);
    assert.ok(existsSync(path), `${where} embeds ${relative(root, path)}, which does not exist`);
  }
});
