import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const core = readFileSync(new URL('../public/src/js/core.js', import.meta.url), 'utf8');
const settle = () => new Promise(resolve => setImmediate(resolve));

function setup({ view = 'main', pending = true, write = async () => false, otherCard = false } = {}) {
  const calls = [], keys = new Map(), classes = new Set(), attributes = new Map();
  const yes = { disabled: false }, no = { disabled: false, focus() {} }, close = { disabled: false };
  const error = { textContent: '' };
  const promo = {
    classList: { add: name => classes.add(name), remove: name => classes.delete(name) },
    setAttribute: (key, value) => attributes.set(key, value),
    querySelector: selector => ({ '.pluginFocusEnable': yes, '.pluginFocusKeep': no,
      '.pluginFocusClose': close, '.pluginFocusError': error })[selector],
  };
  let restoredFocus = 0;
  const document = {
    readyState: 'loading', activeElement: { focus: () => restoredFocus++ },
    addEventListener: (event, handler) => keys.set(event, handler),
    removeEventListener: event => keys.delete(event),
    querySelector: selector => selector === '#pluginFocusPromo' ? promo : otherCard ? {} : null,
  };
  const window = { A2_VIEW: view, addEventListener() {}, __TAURI__: { core: {
    invoke: async (command, args) => {
      assert.equal(command, 'plugin_focus_defaults');
      calls.push(args.accept);
      return args.accept === null ? pending : write(args.accept);
    },
  } } };
  const context = vm.createContext({ window, document, setTimeout: fn => { fn(); return 1; }, console });
  vm.runInContext(core, context);
  const app = vm.runInContext('Object.create(DpsApp.prototype)', context);
  app.safeSetSetting = () => assert.fail('The native one-time decision must not be duplicated in UI storage');
  return { app, yes, no, close, promo, error, calls, classes, attributes, keys,
    restoredFocus: () => restoredFocus };
}

test('an answered or inapplicable prompt does not change settings', async () => {
  const ui = setup({ pending: false });
  assert.equal(await ui.app.maybeShowPluginFocusPromo(), false);
  assert.deepEqual(ui.calls, [null]);
  assert.equal(ui.classes.size, 0);
  for (const view of ['settings', 'details', 'history']) {
    const other = setup({ view });
    assert.equal(await other.app.maybeShowPluginFocusPromo(), false);
    assert.deepEqual(other.calls, []);
  }
});

test('accept enables individual switches only after explicit confirmation', async () => {
  let resolve;
  const ui = setup({ write: () => new Promise(done => { resolve = done; }) });
  assert.equal(await ui.app.maybeShowPluginFocusPromo(), true);
  assert.deepEqual(ui.calls, [null]);
  assert.equal(ui.attributes.get('aria-hidden'), 'false');
  const answer = ui.yes.onclick();
  assert.equal(ui.yes.disabled, true);
  ui.no.onclick(); // A second click while native settings are being written is ignored.
  assert.deepEqual(ui.calls, [null, true]);
  assert.equal(ui.classes.has('isOpen'), true);
  resolve(false);
  await answer;
  assert.equal(ui.classes.has('isOpen'), false);
  assert.equal(ui.attributes.get('aria-hidden'), 'true');
  assert.equal(ui.keys.has('keydown'), false);
  assert.equal(ui.restoredFocus(), 1);
});

test('Keep, close, Escape and the backdrop all save decline without enabling plugins', async () => {
  for (const action of ['keep', 'close', 'escape', 'outside']) {
    const ui = setup();
    await ui.app.maybeShowPluginFocusPromo();
    if (action === 'keep') await ui.no.onclick();
    if (action === 'close') await ui.close.onclick();
    if (action === 'escape') ui.keys.get('keydown')({ key: 'Escape', stopPropagation() {} });
    if (action === 'outside') ui.promo.onclick({ target: ui.promo });
    await settle();
    assert.deepEqual(ui.calls, [null, false], action);
    assert.equal(ui.classes.has('isOpen'), false, action);
  }
});

test('a failed answer remains visible and can be retried without a local answered marker', async () => {
  let failed = true;
  const ui = setup({ write: async () => { if (failed) throw new Error('unavailable'); return false; } });
  await ui.app.maybeShowPluginFocusPromo();
  await ui.yes.onclick();
  assert.equal(ui.classes.has('isOpen'), true);
  assert.match(ui.error.textContent, /try again/);
  assert.equal(ui.no.disabled, false);
  failed = false;
  await ui.no.onclick();
  assert.deepEqual(ui.calls, [null, true, false]);
  assert.equal(ui.classes.has('isOpen'), false);
});

test('an already open card defers the prompt without consuming the native decision', async () => {
  const ui = setup({ otherCard: true });
  assert.equal(await ui.app.maybeShowPluginFocusPromo(), true, 'reserve the launch so another offer does not stack');
  assert.deepEqual(ui.calls, [null]);
  assert.equal(ui.classes.size, 0);
});

test('startup keeps sharing first, offers plugin focus offline, and avoids stacking offers', async () => {
  const start = core.indexOf('    Promise.resolve(window.javaBridge?.discordActivityAvailable?.())');
  const end = core.indexOf('    this.initStreamOverlaySettings();', start);
  assert.ok(start >= 0 && end > start);
  const startup = core.slice(start, end);
  for (const [online, sharing, focus, expected] of [
    [false, false, true, ['focus']],
    [true, true, false, ['sharing', 'news']],
    [true, false, true, ['sharing', 'focus']],
    [true, false, false, ['sharing', 'focus', 'news', 'party']],
  ]) {
    const calls = [];
    const app = { storageKeys: {}, safeGetSetting() {},
      maybeShowSharing: async () => { calls.push('sharing'); return sharing; },
      maybeShowPluginFocusPromo: async () => { calls.push('focus'); return focus; },
      maybeShowWhatsNew: async () => { calls.push('news'); return false; },
      maybeShowPartyPromo: () => calls.push('party') };
    vm.runInNewContext(`(function () { ${startup} }).call(app)`, {
      app, window: { A2_VIEW: 'main', a2Build: { online }, javaBridge: {} }, document: {},
    });
    await settle();
    assert.deepEqual(calls, expected);
  }
});
