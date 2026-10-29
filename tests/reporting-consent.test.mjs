import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const core = readFileSync(new URL('../public/src/js/core.js', import.meta.url), 'utf8');
const promoMethod = core.slice(core.indexOf('  maybeShowPartyPromo() {'), core.indexOf('\n  maybeShowDiscordPromo() {'));
const settingsCode = core.slice(core.indexOf('    // A2Tools users in the party see each other'), core.indexOf('    // Run as administrator'));
const keys = {
  partyPresence: 'dpsMeter.partyPresence',
  partyPresenceConsent: 'dpsMeter.partyPresenceConsent',
  partyPromoShown: 'dpsMeter.partyPromoShown',
  shareData: 'dpsMeter.shareData',
};

function setup(stored = {}) {
  const values = { ...stored };
  const timers = [];
  const elements = new Map();
  const control = () => ({ checked: false, listeners: {}, addEventListener(event, fn) { this.listeners[event] = fn; } });
  for (const selector of ['.partyPromoCheckbox', '.partyPromoDone', '.partyPromoClose', '.partyPresenceCheckbox']) elements.set(selector, control());
  const promo = {
    classList: { add() {}, remove() {} },
    setAttribute() {},
    listeners: {},
    querySelector: selector => elements.get(selector),
    addEventListener(event, fn) { this.listeners[event] = fn; },
  };
  const document = {
    listeners: {},
    querySelector: selector => selector === '#partyPromo' ? promo : elements.get(selector),
    addEventListener(event, fn) { this.listeners[event] = fn; },
    removeEventListener(event) { delete this.listeners[event]; },
  };
  const app = {
    storageKeys: keys,
    safeGetSetting: key => values[key],
    safeSetSetting: (key, value) => { values[key] = value; },
    partyPresenceCheckbox: elements.get('.partyPresenceCheckbox'),
    shareDataCheckbox: control(),
  };
  const context = vm.createContext({ app, document, setTimeout: fn => timers.push(fn) });
  vm.runInContext(`app.maybeShowPartyPromo = ({ ${promoMethod} }).maybeShowPartyPromo`, context);
  return { values, app, context, elements, document, promo, show: () => timers.splice(0).forEach(fn => fn()) };
}

test('presence waits for an explicit choice even when the old popup was dismissed', () => {
  for (const stored of [{}, { 'dpsMeter.partyPresence': 'true', 'dpsMeter.partyPromoShown': 'true' }]) {
    const s = setup(stored);
    assert.equal(s.app.maybeShowPartyPromo(), true);
    assert.equal(s.values[keys.partyPresenceConsent], undefined, 'opening a prompt does not grant consent');
    s.show();
    assert.equal(s.elements.get('.partyPromoCheckbox').checked, false);
    assert.equal(s.values[keys.partyPresenceConsent], undefined);
    s.elements.get('.partyPromoCheckbox').checked = true;
    s.elements.get('.partyPromoDone').listeners.click();
    assert.equal(s.values[keys.partyPresence], 'true');
    assert.equal(s.values[keys.partyPresenceConsent], 'true');
  }
});

test('closing, Escape, or the backdrop keep presence off without committing a checked box', () => {
  for (const close of ['button', 'escape', 'backdrop']) {
    const s = setup({ 'dpsMeter.partyPresence': 'true', 'dpsMeter.partyPromoShown': 'true' });
    s.app.maybeShowPartyPromo();
    s.show();
    s.elements.get('.partyPromoCheckbox').checked = true;
    if (close === 'button') s.elements.get('.partyPromoClose').listeners.click();
    if (close === 'escape') s.document.listeners.keydown({ key: 'Escape', stopPropagation() {} });
    if (close === 'backdrop') s.promo.listeners.click({ target: s.promo });
    assert.equal(s.values[keys.partyPresence], 'false');
    assert.equal(s.values[keys.partyPresenceConsent], 'true');
    assert.equal(s.app.maybeShowPartyPromo(), false, 'a declined choice is remembered');
  }
});

test('a saved decline and a deliberate enable do not prompt again', () => {
  for (const stored of [
    { 'dpsMeter.partyPresence': 'false' },
    { 'dpsMeter.partyPresence': 'true', 'dpsMeter.partyPresenceConsent': 'true' },
  ]) assert.equal(setup(stored).app.maybeShowPartyPromo(), false);
});

test('Settings reflects opt-in defaults and records a deliberate presence choice', () => {
  for (const [stored, presence, sharing] of [
    [{}, false, false],
    [{ 'dpsMeter.partyPresence': 'true', 'dpsMeter.shareSupplyRequests': 'true' }, false, false],
    [{ 'dpsMeter.partyPresence': 'true', 'dpsMeter.partyPresenceConsent': 'true', 'dpsMeter.shareData': 'true' }, true, true],
    [{ 'dpsMeter.shareBossTimers': 'true', 'dpsMeter.shareSupplyRequests': 'true' }, false, true],
    [{ 'dpsMeter.shareData': 'yes' }, false, false],
    [{ 'dpsMeter.shareData': '', 'dpsMeter.shareBossTimers': 'true', 'dpsMeter.shareSupplyRequests': 'true' }, false, false],
  ]) {
    const s = setup(stored);
    vm.runInContext(`(function () { ${settingsCode} }).call(app)`, s.context);
    assert.equal(s.app.partyPresenceCheckbox.checked, presence);
    assert.equal(s.app.shareDataCheckbox.checked, sharing);
    s.app.partyPresenceCheckbox.listeners.change({ target: { checked: true } });
    assert.equal(s.values[keys.partyPresence], 'true');
    assert.equal(s.values[keys.partyPresenceConsent], 'true');
    assert.equal(s.values['dpsMeter.guestUpload'], undefined, 'reporting choices do not grant fight upload consent');
  }
});
