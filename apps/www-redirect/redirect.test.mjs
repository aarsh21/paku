import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const source = readFileSync(new URL('./src/index.js', import.meta.url), 'utf8');
const { default: worker } = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
const request = new Request('https://www.example.invalid/docs?x=1');

test('no redirect until an operator configures a valid HTTPS origin', async () => {
  for (const value of ['', undefined, 'invalid', 'http://example.invalid', 'https://a:b@example.invalid', 'https://example.invalid/path', 'https://example.invalid/?token=secret', 'https://example.invalid/#hash']) {
    assert.equal((await worker.fetch(request, { CANONICAL_ORIGIN: value })).status, 503);
  }
});

test('redirect uses the configured host and preserves path/query', async () => {
  const response = await worker.fetch(request, { CANONICAL_ORIGIN: 'https://example.invalid' });
  assert.equal(response.status, 301);
  assert.equal(response.headers.get('location'), 'https://example.invalid/docs?x=1');
});

test('misrouted canonical host cannot loop', async () => {
  assert.equal((await worker.fetch(request, { CANONICAL_ORIGIN: 'https://www.example.invalid' })).status, 508);
});
