import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';

const html = readFileSync(new URL('./public/index.html', import.meta.url), 'utf8');

test('Pi-only landing has explicit upstream credit and source-build instructions', () => {
  assert.match(html, /<title>Paku/);
  assert.match(html, /Pi-only/);
  assert.match(html, /cargo run -p paku/);
  assert.match(html, /cargo run -p paku -- headless/);
  assert.match(html, /independent Pi-only fork of/);
  assert.match(html, /https:\/\/github\.com\/zeronsh\/zeron/);
  assert.match(html, /Zeron contributors/);
  assert.match(html, /not an official Zeron or Pi product/);
});

test('downloads and optional hosting are honest, with no inherited endorsements', () => {
  assert.match(html, /https:\/\/github\.com\/aarsh21\/paku\/releases/);
  assert.match(html, /Prebuilt Paku downloads are not promised/);
  assert.match(html, /no hosted Paku service/);
  assert.doesNotMatch(html, /https?:\/\/(?:[^/]+\.)?(?:zeron|paku)\.sh/);
  assert.doesNotMatch(html, /releases\/latest|\.dmg|setup\.exe|zeron-\d|paku-\d/);
  assert.doesNotMatch(html, /testimonial|sponsor|rauchg|thecontextcompany|discord\.gg|x\.com\//i);
  assert.doesNotMatch(html, /Claude Code|Codex|Cursor|Devin|Grok|Hermes|Antigravity|OpenCode/);
});

test('all external links are repository destinations, local assets exist, no telemetry', () => {
  const hrefs = [...html.matchAll(/href="([^"]+)"/g)].map(match => match[1]);
  for (const href of hrefs.filter(href => href.startsWith('https:'))) {
    assert.ok(href.startsWith('https://github.com/aarsh21/paku') || href === 'https://github.com/zeronsh/zeron', href);
  }
  for (const match of html.matchAll(/(?:href="|url\(')(\/[^"')]+)/g)) {
    assert.ok(existsSync(new URL(`./public${match[1]}`, import.meta.url)), match[1]);
  }
  assert.doesNotMatch(html, /<script|posthog|analytics|app-screenshot/i);
  assert.match(html, /prefers-reduced-motion/);
  assert.match(html, /name="viewport"/);
  assert.match(html, /aria-label="Main navigation"/);
});
