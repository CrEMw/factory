// Path tokens of the agent mailbox: node --test, no build (pnpm run test).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { gateOpen, pathTokens } from '#lib/server/mailbox-gate';

test('pathTokens: comma list with spaces and empty items; unset or empty is no token', () => {
  assert.deepEqual(pathTokens({ MAILBOX_PATH_TOKENS: ' new-token , old-token,,' }), ['new-token', 'old-token']);
  assert.deepEqual(pathTokens({ MAILBOX_PATH_TOKENS: '' }), []);
  assert.deepEqual(pathTokens({}), []);
});

test('gateOpen: both tokens of a rotation window open, an unknown one or a prefix does not, no tokens close all', () => {
  const tokens = pathTokens({ MAILBOX_PATH_TOKENS: 'new-token,old-token' });
  assert.equal(gateOpen('new-token', tokens), true);
  assert.equal(gateOpen('old-token', tokens), true);
  assert.equal(gateOpen('other-token', tokens), false);
  assert.equal(gateOpen('new-toke', tokens), false);
  assert.equal(gateOpen('', tokens), false);
  assert.equal(gateOpen('new-token', []), false);
});
