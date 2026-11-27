import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readSeen, SEEN_KEY, writeSeen } from '#lib/agents-seen';

const memory = (init: Record<string, string> = {}) => {
  const m = new Map(Object.entries(init));
  return { m, getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) };
};

test('seen: read and write the one key', () => {
  assert.equal(readSeen(memory()), 0);
  assert.equal(readSeen(memory({ [SEEN_KEY]: '45' })), 45);
  const s = memory();
  writeSeen(s, 50);
  assert.deepEqual([...s.m], [[SEEN_KEY, '50']]);
});
