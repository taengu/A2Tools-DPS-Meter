import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../public/src/js/pluginFrame.js', import.meta.url), 'utf8');

async function host({ docked = false, beforeHello, invoke } = {}) {
  const calls = [], sent = [], listeners = new Map(), stored = new Map();
  const frame = { setAttribute() {}, remove() {}, contentWindow: { postMessage: message => sent.push(message) } };
  const window = {
    __TAURI__: { core: {
      convertFileSrc: () => 'http://a2plugin.localhost/',
      invoke: async (name, args) => {
        calls.push({ name, args });
        if (name === 'plugin_hello' && beforeHello) await beforeHello(plugin);
        if (name !== 'plugin_hello' && invoke) return invoke(name, args);
        return name === 'plugin_hello' ? {
          manifest: { permissions: ['decoder:pets'] }, run: 'run:3', generation: 3, replay: [],
        } : true;
      },
    } },
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: name => listeners.delete(name),
  };
  vm.runInNewContext(source, {
    window, document: { createElement: () => frame }, URL, Date,
    localStorage: { getItem: key => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) },
  });
  const plugin = window.createPluginFrame({ id: 'pets', docked, stage: { appendChild() {} } });
  await plugin.start();
  const message = (call, args = [], source = frame.contentWindow) => listeners.get('message')({
    source, data: { a2: 1, id: 'request', call, args },
  });
  await message('ready');
  return { plugin, calls, sent, message, stored };
}

test('a plugin link uses the native confirmation command and preserves its dock identity', async () => {
  const page = await host({ docked: true });
  const url = 'https://example.com/path?data=visible-in-confirmation';
  await page.message('openUrl', [url], {});
  assert.equal(page.calls.filter(call => call.name === 'plugin_open_url').length, 0);
  await page.message('openUrl', [url]);
  const request = page.calls.find(call => call.name === 'plugin_open_url');
  assert.equal(request.args.plugin, 'pets');
  assert.equal(request.args.url, url);
  assert.equal(page.calls.some(call => call.name === 'open_url'), false);
});

test('old delivery epochs cannot replay activity into a newly enabled plugin or advance its cursor', async () => {
  const page = await host();
  const event = (generation, seq, value) => ({ event: 'decoded:pets', generation, seq, data: { value } });
  page.plugin.receive(event(3, 100, 'current'));
  page.plugin.receive(event(1, 1000, 'before-disable'));
  page.plugin.receive(event(3, 101, 'next'));
  page.plugin.receive({ event: 'ping', generation: 1, data: { ms: 900 } });
  page.plugin.receive({ event: 'ping', generation: 3, data: { ms: 20 } });
  const decoded = page.sent.filter(message => message.event === 'decoded:pets');
  assert.deepEqual(decoded.map(message => message.data.value), ['current', 'next']);
  assert.equal(JSON.parse(page.stored.get('a2plugin.cursor.pets')).seq, 101);
  assert.deepEqual(page.sent.filter(message => message.event === 'ping').map(message => message.data.ms), [20]);
});

test('unnumbered activity arriving before hello keeps its original epoch', async () => {
  const page = await host({ beforeHello(plugin) {
    plugin.receive({ event: 'ping', generation: 1, data: { ms: 900 } });
    plugin.receive({ event: 'ping', generation: 3, data: { ms: 20 } });
  } });
  assert.deepEqual(page.sent.filter(message => message.event === 'ping').map(message => message.data.ms), [20]);
});

test('a frame cannot accumulate unlimited native calls or forward oversized requests', async () => {
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  const page = await host({ invoke: () => pending });
  const requests = Array.from({ length: 80 }, () => page.message('storage.get', ['key']));
  assert.equal(page.calls.filter(call => call.name === 'plugin_storage_get').length, 64);
  assert.ok(page.sent.some(message => /too many/.test(message.error ?? '')));
  release(null);
  await Promise.all(requests);
  await page.message('storage.set', ['key', 'x'.repeat(3 * 1024 * 1024)]);
  assert.equal(page.calls.some(call => call.name === 'plugin_storage_set'), false);
  assert.ok(page.sent.some(message => /too large/.test(message.error ?? '')));
});
