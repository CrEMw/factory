import assert from 'node:assert/strict';
import test from 'node:test';
import { documentHref } from '../hub-web/src/lib/document-url.ts';

test('same relative path opens its own repository and preserves path segments', () => {
  const path = 'docs/superpowers/plans/δοκιμή #?.md';
  const links = ['acme/apps', 'acme/platform'].map((repo) => {
    const href = documentHref(repo, path);
    const url = new URL(href, 'https://hub.example.invalid');
    assert.equal(url.searchParams.get('repo'), repo);
    assert.equal(decodeURIComponent(url.pathname), '/d/' + path);
    assert.equal(url.hash, '');
    return href;
  });
  assert.notEqual(...links);
});
