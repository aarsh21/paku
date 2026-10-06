#!/usr/bin/env node
// Real Linux desktop -> engine IPC -> genuine Pi CLI -> rendered transcript.
// Requires Xvfb, xdotool, ffmpeg, ImageMagick and installed Pi. No paid API.
import { spawn, execFileSync } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdirSync, mkdtempSync, writeFileSync, copyFileSync, readFileSync, existsSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import assert from 'node:assert/strict';

const root = resolve(import.meta.dirname, '..');
const output = resolve(process.argv[2] || '/tmp/paku-native-ui');
mkdirSync(output, { recursive: true });
const xdotool = process.env.XDOTOOL || 'xdotool';
const xvfb = process.env.XVFB || 'Xvfb';
const binary = process.env.PAKU_BINARY || join(root, 'target/debug/paku');
const uiFontSize = Number(process.env.PAKU_NATIVE_UI_FONT_SIZE || 16);
const uiScale = Number(process.env.PAKU_NATIVE_UI_SCALE || 1);
const useWayland = process.env.PAKU_NATIVE_USE_WAYLAND === '1';
assert.ok([12, 13, 14, 15, 16, 18, 20, 24, 28, 32].includes(uiFontSize), 'supported native UI font size');
assert.ok(Number.isFinite(uiScale) && uiScale >= 0.75 && uiScale <= 2, 'supported full interface scale');
const processes = [];
const pause = ms => new Promise(r => setTimeout(r, ms));
const deadline = async (fn, label, seconds = 45) => {
  const end = Date.now() + seconds * 1000;
  while (Date.now() < end) {
    const value = await fn();
    if (value) return value;
    await pause(100);
  }
  throw new Error(`Timed out waiting for ${label}`);
};
function start(command, args, env, log) {
  const child = spawn(command, args, { env, stdio: ['ignore', 'pipe', 'pipe'] });
  processes.push(child);
  child.stdout.on('data', b => writeFileSync(join(output, log), b, { flag: 'a' }));
  child.stderr.on('data', b => writeFileSync(join(output, log), b, { flag: 'a' }));
  child.on('error', e => writeFileSync(join(output, log), `${e}\n`, { flag: 'a' }));
  return child;
}
const freePort = () => new Promise((resolvePort, reject) => {
  const server = createServer();
  server.on('error', reject);
  server.listen(0, '127.0.0.1', () => { const port = server.address().port; server.close(() => resolvePort(port)); });
});
let socket;
try {
  const sandbox = mkdtempSync(join(output, 'sandbox-'));
  const agent = join(sandbox, 'agent');
  const workspace = join(sandbox, 'workspace');
  mkdirSync(join(sandbox, 'data'), { recursive: true });
  writeFileSync(join(sandbox, 'data/ui-settings.json'), JSON.stringify({ uiFontSize, uiScale }));
  mkdirSync(join(agent, 'extensions'), { recursive: true });
  mkdirSync(workspace, { recursive: true });
  writeFileSync(join(workspace, 'README.md'), '# Native Paku E2E\n');
  writeFileSync(join(agent, 'settings.json'), '{"retry":{"enabled":false}}');
  copyFileSync(join(root, 'crates/harness/tests/fixtures/pi-rpc-probe.ts'), join(agent, 'extensions/probe.ts'));
  const pi = execFileSync('which', ['pi'], { encoding: 'utf8' }).trim();
  const quote = s => `'${s.replaceAll("'", "'\\''")}'`;
  const wrapper = join(sandbox, 'native-pi');
  writeFileSync(wrapper, `#!/bin/sh\nexport PI_CODING_AGENT_DIR=${quote(agent)}\nexec ${quote(pi)} "$@"\n`, { mode: 0o700 });
  const displayFile = join(output, 'display.txt');
  // -displayfd allocates a private display, never interacting with the human's desktop.
  const displayFd = (await import('node:fs')).openSync(displayFile, 'w');
  const x = spawn(xvfb, ['-displayfd', '3', '-screen', '0', '1280x800x24', '-ac', '-nolisten', 'tcp'],
    { stdio: ['ignore', 'pipe', 'pipe', displayFd] });
  processes.push(x);
  x.stderr.on('data', b => writeFileSync(join(output, 'xvfb.log'), b, { flag: 'a' }));
  const display = await deadline(() => { const n = readFileSync(displayFile, 'utf8').trim(); return /^\d+$/.test(n) ? `:${n}` : null; }, 'private X display');
  const port = await freePort();
  const env = { DISPLAY: display, WAYLAND_DISPLAY: '', PAKU_DATA_DIR: join(sandbox, 'data'),
    PAKU_IPC_PORT: `${port}`, PAKU_EDGE_URL: '', PAKU_WORKOS_CLIENT_ID: '', PAKU_EDGE_TOKEN: '',
    PAKU_HARNESS: 'pi', PI_EXECUTABLE: wrapper, PI_CODING_AGENT_DIR: agent, RUST_LOG: 'info' };
  // Empty inherited edge tokens must not enable development sync.
  delete env.PAKU_EDGE_TOKEN;
  const childEnv = { ...process.env, ...env };
  delete childEnv.PAKU_EDGE_TOKEN;
  delete childEnv.PAKU_RELEASES_URL;
  let compositor;
  if (useWayland) {
    const runtime = join(sandbox, 'runtime');
    mkdirSync(runtime, { recursive: true, mode: 0o700 });
    childEnv.XDG_RUNTIME_DIR = runtime;
    childEnv.WAYLAND_DISPLAY = 'paku-private-scale';
    childEnv.GDK_BACKEND = 'wayland';
    const moduleRoot = process.env.PAKU_WESTON_MODULE_ROOT;
    const westonEnv = { ...childEnv, WAYLAND_DISPLAY: '' };
    if (moduleRoot) westonEnv.WESTON_MODULE_MAP = `x11-backend.so=${moduleRoot}/x11-backend.so`;
    compositor = start(process.env.PAKU_WESTON_BINARY || 'weston', [
      '--backend=x11', '--renderer=pixman',
      `--shell=${process.env.PAKU_WESTON_SHELL || 'kiosk-shell.so'}`,
      '--socket=paku-private-scale', '--no-config', '--width=1200', '--height=760',
    ], westonEnv, 'weston.log');
    await deadline(() => {
      if (compositor.exitCode !== null) throw new Error('Private Weston exited; see weston.log');
      return existsSync(join(runtime, 'paku-private-scale'));
    }, 'private Wayland compositor');
  }
  start(binary, ['headless'], childEnv, 'engine.log');
  socket = await deadline(async () => {
    const s = new WebSocket(`ws://127.0.0.1:${port}`);
    const connected = await new Promise(r => { s.onopen = () => r(true); s.onerror = () => r(false); });
    if (connected) return s;
    s.close(); return null;
  }, 'Paku engine IPC');
  let nextId = 1;
  const pending = new Map();
  const streams = new Map();
  const frames = [];
  socket.onmessage = ({ data }) => {
    const frame = JSON.parse(data);
    frames.push(frame);
    const p = pending.get(frame.id);
    if (p && Object.hasOwn(frame, 'ok')) { pending.delete(frame.id); p.resolve(frame.ok); }
    else if (p && frame.err) { pending.delete(frame.id); p.reject(new Error(JSON.stringify(frame))); }
    const stream = streams.get(frame.id);
    if (stream && Object.hasOwn(frame, 'item')) stream.push(frame.item);
  };
  const call = (method, params = {}) => new Promise((resolveCall, reject) => {
    const id = nextId++;
    pending.set(id, { resolve: resolveCall, reject });
    socket.send(JSON.stringify({ id, method, params }));
    setTimeout(() => { if (pending.delete(id)) reject(new Error(`RPC timeout: ${method}`)); }, 30000).unref();
  });
  await call('EngineReady');
  const engineInfo = await call('EngineInfo');
  assert.equal(engineInfo.workspaceScope, 'local');
  assert.ok(engineInfo.capabilities.includes('paku-pi-only-v1'));
  const catalog = await call('ListHarnesses');
  assert.equal(catalog.length, 1);
  assert.equal(catalog[0].id, 'pi');
  assert.equal(catalog[0].installed, true);
  const device = await call('LocalDevice');
  const chat = `native-ui-proof-${randomUUID()}`;
  const input = `native Paku UI proof ${randomUUID().slice(0, 8)}`;
  await call('Mutate', { op: 'createSpace', spaceId: 'native-ui-space', deviceId: device.deviceId, path: workspace });
  await call('Mutate', { op: 'createChat', chatId: chat, spaceId: 'native-ui-space', config: { harness: 'pi', model: 'paku-probe/mock', sandbox: 'workspace-write', reasoning: null } });
  await call('Mutate', { op: 'renameChat', chatId: chat, title: 'Paku native Pi E2E' });
  const updates = [];
  const subId = nextId++;
  streams.set(subId, updates);
  socket.send(JSON.stringify({ id: subId, method: 'WatchDocMessages', params: { chatId: chat } }));
  const locator = createHash('sha256').update(`Local\0device:${device.deviceId}`).digest('hex').slice(0, 16);
  const video = start('ffmpeg', ['-nostdin', '-loglevel', 'error', '-y', '-f', 'x11grab', '-framerate', '15', '-video_size', '1280x800', '-i', display, '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '23', '-pix_fmt', 'yuv420p', join(output, 'native-ui.mp4')], childEnv, 'video.log');
  const ui = start(binary, [`paku://open/chat/${chat}?workspace=${locator}`], childEnv, 'ui.log');
  const runX = args => execFileSync(xdotool, args, { env: childEnv, encoding: 'utf8' }).trim();
  const window = await deadline(() => {
    try { return runX(compositor
      ? ['search', '--onlyvisible', '--class', 'weston']
      : ['search', '--onlyvisible', '--pid', `${ui.pid}`]).split('\n')[0]; }
    catch { if (ui.exitCode !== null) throw new Error(`Paku UI exited: ${ui.exitCode}; see ui.log`); return null; }
  }, 'native Paku window');
  runX(['windowsize', window, '1200', '760']);
  runX(['windowmove', window, '0', '0']);
  runX(['windowfocus', '--sync', window]);
  await pause(3000); // Native render/animation settling; transcript assertion below is event-driven.
  execFileSync('magick', ['import', '-display', display, '-window', window, join(output, '01-conversation.png')]);
  runX(['mousemove', '--window', window,
    process.env.PAKU_NATIVE_COMPOSER_X || '720',
    process.env.PAKU_NATIVE_COMPOSER_Y || `${Math.round(760 - 66 * uiScale)}`]);
  runX(['click', '1']);
  runX(['type', '--clearmodifiers', '--delay', '40', input]);
  runX(['key', '--clearmodifiers', 'Return']);
  try {
    await deadline(() => JSON.stringify(updates).includes(`MOCK:${input}`), 'real Pi reply to actual desktop keyboard input');
  } catch (error) {
    execFileSync('magick', ['import', '-display', display, '-window', window, join(output, 'failure.png')]);
    writeFileSync(join(output, 'failed-events.json'), JSON.stringify(updates, null, 2));
    throw error;
  }
  assert.equal(JSON.stringify(updates).includes('"kind":"error"'), false, 'genuine Pi/MCP must not emit error parts');
  await pause(1000);
  execFileSync('magick', ['import', '-display', display, '-window', window, join(output, '02-pi-reply.png')]);
  const zoomChecks = [];
  if (process.env.PAKU_NATIVE_TEST_ZOOM_SHORTCUTS === '1') {
    const checkZoom = async (key, expected, name) => {
      runX(['key', '--clearmodifiers', key]);
      await deadline(() => JSON.parse(readFileSync(join(sandbox, 'data/ui-settings.json'), 'utf8')).uiScale === expected, `saved zoom ${expected}`);
      await pause(500);
      execFileSync('magick', ['import', '-display', display, '-window', window, join(output, name)]);
      const saved = JSON.parse(readFileSync(join(sandbox, 'data/ui-settings.json'), 'utf8'));
      assert.equal(saved.uiFontSize, uiFontSize, 'zoom must not change font preferences');
      zoomChecks.push({ key, expected, uiFontSize: saved.uiFontSize });
    };
    await checkZoom('ctrl+plus', Math.min(2, uiScale + 0.25), '03-zoom-in.png');
    await checkZoom('ctrl+minus', Math.max(0.75, Math.min(2, uiScale + 0.25) - 0.25), '04-zoom-out.png');
    await checkZoom('ctrl+0', 1, '05-zoom-reset.png');
    await checkZoom('ctrl+equal', 1.25, '06-zoom-equal.png');
    await checkZoom('ctrl+0', 1, '07-zoom-final-reset.png');
  }
  const proof = { timestamp: new Date().toISOString(), binary, piExecutable: pi, catalog, chat,
    input, uiFontSize, uiScale, useWayland, zoomChecks, expectedReply: `MOCK:${input}`, engineInfo,
    transport: `native ${useWayland ? 'Wayland via private nested Weston' : 'X11'} keyboard -> production desktop -> engine WebSocket IPC -> genuine installed Pi with local model fixture`, updates, frames };
  writeFileSync(join(output, 'native-ui-proof.json'), JSON.stringify(proof, null, 2));
  video.kill('SIGINT');
  await new Promise(r => video.once('exit', r));
  ui.kill('SIGTERM');
  await call('StopEngine');
  console.log(`PASS: genuine Pi reply received from native Paku keyboard send; evidence: ${output}`);
} finally {
  socket?.close();
  for (const process of processes.reverse()) { if (process.exitCode === null) process.kill('SIGTERM'); }
}
